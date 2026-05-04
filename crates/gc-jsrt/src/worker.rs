//! Dedicated worker thread that owns a single [`rquickjs::Runtime`].
//!
//! The worker is fed jobs via [`std::sync::mpsc`] (cheap, blocking on
//! the worker side) and replies through [`tokio::sync::oneshot`] so
//! callers in async code don't need to block. We deliberately do not
//! use rquickjs' `AsyncRuntime`: the JS we run is short, synchronous
//! post-processing logic, so a sync runtime on a dedicated thread is
//! the smaller, simpler primitive.
//!
//! Concurrency model:
//! - One [`JsWorker`] owns one OS thread and one runtime.
//! - Multiple Tokio tasks may call [`JsWorker::evaluate`] concurrently;
//!   the channel serialises them onto the worker.
//! - The runtime is reused across jobs (warm GC, no allocator churn).
//! - The [`rquickjs::Context`] is **fresh per job** so two unrelated
//!   specs cannot pollute each other's globals.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rquickjs::{CatchResultExt, Context, Promise, Runtime, Value};
use tokio::sync::oneshot;

pub use crate::host::MAX_SHELL_CALLS_PER_EVALUATION;
use crate::host::{install_host_api, HostState};
use crate::normalize::normalize_value;
use crate::sandbox::configure_or_internal;
use crate::types::{
    JsDiagnostic, JsDiagnosticCode, JsExecutionKind, JsRuntimeError, JsRuntimeInput,
    JsRuntimeOutput, JsRuntimeOutputPayload,
};

/// Hard cap on QuickJS heap usage per worker, in bytes (8 MiB).
///
/// The corpus rarely allocates beyond a few KB; this gives ~1000x
/// headroom while still terminating runaway scripts cleanly.
const MEMORY_LIMIT_BYTES: usize = 8 * 1024 * 1024;

/// Maximum JS stack size, in bytes (512 KiB).
///
/// QuickJS' default is 256 KiB; we double it because our outer call
/// (`JSON.stringify`) plus user code can hit deeper stacks than the
/// default expects.
const MAX_STACK_SIZE_BYTES: usize = 512 * 1024;

/// Heap threshold that triggers a GC cycle (2 MiB).
const GC_THRESHOLD_BYTES: usize = 2 * 1024 * 1024;

/// One job submitted to the worker.
struct Job {
    program: String,
    input: JsRuntimeInput,
    deadline: Instant,
    reply: oneshot::Sender<Result<JsRuntimeOutput, JsRuntimeError>>,
}

/// Public handle to the worker thread.
///
/// Cheap to clone — internally just an `Arc` over the channel sender.
#[derive(Clone)]
pub struct JsWorker {
    sender: mpsc::Sender<Job>,
    /// Held only so the worker thread is joined when the last clone is
    /// dropped. Wrapped in `Arc` to make `JsWorker: Clone`.
    _handle: Arc<WorkerHandle>,
}

struct WorkerHandle {
    thread: Option<JoinHandle<()>>,
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        // The worker loop exits on channel disconnect (which fires when
        // the last `JsWorker.sender` clone drops). We can't drop the
        // sender here because `JsWorker` is `Clone`; we just join.
        if let Some(thread) = self.thread.take() {
            match thread.join() {
                Ok(()) => {}
                Err(payload) => {
                    let msg = payload
                        .downcast_ref::<&'static str>()
                        .map(|s| (*s).to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "<non-string panic payload>".into());
                    tracing::error!(
                        panic = %msg,
                        "gc-jsrt worker thread panicked during shutdown"
                    );
                }
            }
        }
    }
}

impl JsWorker {
    /// Spawn the worker thread. Returns once the thread has started.
    ///
    /// This may return [`JsRuntimeError::Internal`] if the OS rejects
    /// the thread spawn or if the runtime cannot be created.
    pub fn spawn() -> Result<Self, JsRuntimeError> {
        let (tx, rx) = mpsc::channel::<Job>();

        let thread = thread::Builder::new()
            .name("gc-jsrt-worker".into())
            .spawn(move || {
                if let Err(err) = worker_main(rx) {
                    tracing::error!(error = %err, "gc-jsrt worker thread exited with error");
                }
            })
            .map_err(|e| JsRuntimeError::Internal(format!("spawn worker: {e}")))?;

        Ok(Self {
            sender: tx,
            _handle: Arc::new(WorkerHandle {
                thread: Some(thread),
            }),
        })
    }

    /// Evaluate `program` with a wall-clock budget of `timeout`.
    ///
    /// Returns [`JsRuntimeError::WorkerDead`] if the worker thread is
    /// gone. All other failure modes (timeout, oversized output,
    /// invalid shape, exception) are reported as
    /// [`crate::JsDiagnostic`]s on a successful [`JsRuntimeOutput`].
    pub async fn evaluate(
        &self,
        program: impl Into<String>,
        input: JsRuntimeInput,
        timeout: Duration,
    ) -> Result<JsRuntimeOutput, JsRuntimeError> {
        let deadline = Instant::now() + timeout;
        let (reply_tx, reply_rx) = oneshot::channel();
        let job = Job {
            program: program.into(),
            input,
            deadline,
            reply: reply_tx,
        };
        self.sender
            .send(job)
            .map_err(|_| JsRuntimeError::WorkerDead)?;
        reply_rx.await.map_err(|_| JsRuntimeError::WorkerDead)?
    }

    /// Test-only helper that returns a worker whose thread exits
    /// immediately, leaving the channel disconnected. Used to assert
    /// the recovery path on a dead worker.
    #[doc(hidden)]
    pub fn spawn_for_test_with_failing_thread() -> Result<Self, JsRuntimeError> {
        let (tx, rx) = mpsc::channel::<Job>();
        let thread = thread::Builder::new()
            .name("gc-jsrt-worker-failing".into())
            .spawn(move || {
                drop(rx);
            })
            .map_err(|e| JsRuntimeError::Internal(format!("spawn worker: {e}")))?;
        // Wait for the thread to finish so the receiver is definitely
        // dropped before any caller tries to send.
        let _ = thread.join();
        Ok(Self {
            sender: tx,
            _handle: Arc::new(WorkerHandle { thread: None }),
        })
    }
}

/// Enclosed error type so the worker can fail-fast at startup.
#[derive(Debug)]
enum WorkerStartupError {
    Runtime(rquickjs::Error),
}

impl std::fmt::Display for WorkerStartupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Runtime(e) => write!(f, "rquickjs runtime: {e}"),
        }
    }
}

/// Top-level worker loop. Returns when the channel disconnects (i.e.
/// every [`JsWorker`] clone has been dropped).
fn worker_main(rx: mpsc::Receiver<Job>) -> Result<(), WorkerStartupError> {
    let runtime = Runtime::new().map_err(WorkerStartupError::Runtime)?;
    runtime.set_memory_limit(MEMORY_LIMIT_BYTES);
    runtime.set_max_stack_size(MAX_STACK_SIZE_BYTES);
    runtime.set_gc_threshold(GC_THRESHOLD_BYTES);

    // Wall-clock interrupt: shared deadline; the handler returns true
    // when `now()` is past the stored value. The deadline is updated
    // per-job before evaluation begins.
    //
    // Storing `i64` nanoseconds since some epoch (we use process start)
    // keeps the read in the interrupt path lock-free. A sentinel of
    // `i64::MAX` means "no deadline" (no JS currently running).
    let process_start = Instant::now();
    let deadline_relative_ns = Arc::new(AtomicI64::new(i64::MAX));
    let interrupted_flag = Arc::new(AtomicBool::new(false));

    {
        let deadline = deadline_relative_ns.clone();
        let interrupted = interrupted_flag.clone();
        runtime.set_interrupt_handler(Some(Box::new(move || {
            let limit = deadline.load(Ordering::Relaxed);
            if limit == i64::MAX {
                return false;
            }
            let elapsed = process_start.elapsed().as_nanos() as i64;
            if elapsed >= limit {
                // Latch the flag so the post-eval branch can tell a
                // timeout apart from a regular exception.
                interrupted.store(true, Ordering::SeqCst);
                return true;
            }
            false
        })));
    }

    while let Ok(job) = rx.recv() {
        let result = run_job(
            &runtime,
            &job,
            &deadline_relative_ns,
            &interrupted_flag,
            process_start,
        );
        let _ = job.reply.send(result);
    }

    Ok(())
}

/// Run one job. The runtime is reused, the context is fresh.
fn run_job(
    runtime: &Runtime,
    job: &Job,
    deadline: &AtomicI64,
    interrupted: &AtomicBool,
    process_start: Instant,
) -> Result<JsRuntimeOutput, JsRuntimeError> {
    // Reset the timeout latch and arm the interrupt handler.
    interrupted.store(false, Ordering::SeqCst);
    let deadline_ns = deadline_relative_to(process_start, job.deadline);
    deadline.store(deadline_ns, Ordering::Relaxed);

    // Always disarm the deadline before returning so a subsequent
    // synchronous worker operation (e.g. GC) can't be aborted by it.
    let _disarm = DeadlineGuard {
        deadline,
        atomic: i64::MAX,
    };

    let context = match Context::full(runtime) {
        Ok(c) => c,
        Err(e) => {
            return Ok(JsRuntimeOutput::empty_with(JsDiagnostic {
                code: JsDiagnosticCode::Exception,
                message: format!("could not create context: {e}"),
            }));
        }
    };

    let timeout_ms = (job.deadline.saturating_duration_since(Instant::now())).as_millis() as u64;

    let host_state = HostState::new();
    let host_state_for_post = host_state.clone();

    Ok(context.with(|ctx| -> JsRuntimeOutput {
        if let Err(msg) = configure_or_internal(&ctx) {
            return JsRuntimeOutput::empty_with(JsDiagnostic {
                code: JsDiagnosticCode::Exception,
                message: msg,
            });
        }

        // Host bindings install unconditionally because the per-job
        // context is fresh — even PostProcess jobs that never touch them
        // pay only a few property sets.
        if let Err(e) = install_host_api(&ctx, &job.input, host_state.clone(), job.deadline) {
            return JsRuntimeOutput::empty_with(JsDiagnostic {
                code: JsDiagnosticCode::Exception,
                message: format!("could not install host API: {e}"),
            });
        }

        // Evaluate. We accept either a synchronous value or a Promise
        // (corpus uses both `(out) => [...]` and `async (out) => [...]`).
        let value: Value<'_> = match ctx.eval::<Value<'_>, _>(job.program.as_bytes()).catch(&ctx) {
            Ok(v) => v,
            Err(e) => {
                let mut out = classify_error(e, interrupted, timeout_ms);
                attach_host_diagnostics(&mut out, &host_state_for_post);
                return out;
            }
        };

        // If the result is a Promise, finish it synchronously. Promise
        // resolution may itself trigger more JS, all of which is still
        // bounded by the same interrupt handler.
        let resolved = if value.as_promise().is_some() {
            let promise: Promise<'_> = match value.into_promise() {
                Some(p) => p,
                None => unreachable!("as_promise() said yes"),
            };
            match promise.finish::<Value<'_>>().catch(&ctx) {
                Ok(v) => v,
                Err(e) => {
                    let mut out = classify_error(e, interrupted, timeout_ms);
                    attach_host_diagnostics(&mut out, &host_state_for_post);
                    return out;
                }
            }
        } else {
            value
        };

        let mut output = match job.input.kind {
            JsExecutionKind::PostProcess | JsExecutionKind::Custom => {
                normalize_value(&ctx, resolved)
            }
            JsExecutionKind::ScriptFunction => normalize_argv(&ctx, resolved),
        };
        attach_host_diagnostics(&mut output, &host_state_for_post);
        output
    }))
}

/// Surface unsupported-host-api accumulator into the runtime output.
fn attach_host_diagnostics(output: &mut JsRuntimeOutput, state: &HostState) {
    for name in state.drain_unsupported() {
        output.diagnostics.push(JsDiagnostic {
            code: JsDiagnosticCode::UnsupportedHostApi,
            message: name,
        });
    }
}

/// Convert a JS return value into an argv vector for `script_function`
/// generators. Accepts either:
///   * `["cmd", "arg1", "arg2"]`            – plain argv array
///   * `{ command: "cmd", args: ["arg1"] }` – Fig structured descriptor
///
/// Anything else surfaces as `InvalidArgv`. On success the returned
/// output carries [`JsRuntimeOutputPayload::Argv`]; on any failure it
/// carries [`JsRuntimeOutputPayload::None`] with the explanation in
/// `diagnostics`. The Suggestions variant is intentionally never used
/// here — `script_function` produces no suggestions of its own; those
/// come from the follow-up script invocation in the engine.
fn normalize_argv<'js>(_ctx: &rquickjs::Ctx<'js>, value: Value<'js>) -> JsRuntimeOutput {
    let mut diagnostics: Vec<JsDiagnostic> = Vec::new();

    // Accept both `["cmd", "arg"]` and `{ command, args }`.
    if let Some(arr) = value.as_array() {
        let mut argv: Vec<String> = Vec::with_capacity(arr.len());
        for i in 0..arr.len() {
            let v: Value<'js> = match arr.get(i) {
                Ok(v) => v,
                Err(e) => {
                    diagnostics.push(JsDiagnostic {
                        code: JsDiagnosticCode::InvalidArgv,
                        message: format!("argv element [{i}] read failed: {e}"),
                    });
                    return JsRuntimeOutput {
                        payload: JsRuntimeOutputPayload::None,
                        diagnostics,
                    };
                }
            };
            let Some(s) = v.as_string() else {
                diagnostics.push(JsDiagnostic {
                    code: JsDiagnosticCode::InvalidArgv,
                    message: format!("argv element [{i}] is not a string"),
                });
                return JsRuntimeOutput {
                    payload: JsRuntimeOutputPayload::None,
                    diagnostics,
                };
            };
            match s.to_string() {
                Ok(s) => argv.push(s),
                Err(e) => {
                    diagnostics.push(JsDiagnostic {
                        code: JsDiagnosticCode::InvalidArgv,
                        message: format!("argv element [{i}] decode failed: {e}"),
                    });
                    return JsRuntimeOutput {
                        payload: JsRuntimeOutputPayload::None,
                        diagnostics,
                    };
                }
            }
        }
        if argv.is_empty() {
            diagnostics.push(JsDiagnostic {
                code: JsDiagnosticCode::InvalidArgv,
                message: "argv array must have at least one element".into(),
            });
            return JsRuntimeOutput {
                payload: JsRuntimeOutputPayload::None,
                diagnostics,
            };
        }
        return JsRuntimeOutput {
            payload: JsRuntimeOutputPayload::Argv(argv),
            diagnostics,
        };
    }

    if let Some(obj) = value.as_object() {
        let mut argv: Vec<String> = Vec::new();
        // Mirror the host.rs hardening for `executeShellCommand` descriptor
        // inspection: a Proxy with a throwing `has` trap (or any other
        // failure inside `contains_key`) must surface as a typed
        // InvalidArgv diagnostic rather than silently producing an empty
        // argv slot. Keeps the host-API contract uniform across both
        // call sites.
        let has_command = match obj.contains_key("command") {
            Ok(b) => b,
            Err(e) => {
                diagnostics.push(JsDiagnostic {
                    code: JsDiagnosticCode::InvalidArgv,
                    message: format!("failed to inspect descriptor.command: {e}"),
                });
                return JsRuntimeOutput {
                    payload: JsRuntimeOutputPayload::None,
                    diagnostics,
                };
            }
        };
        if has_command {
            let v: Value<'js> = match obj.get("command") {
                Ok(v) => v,
                Err(e) => {
                    diagnostics.push(JsDiagnostic {
                        code: JsDiagnosticCode::InvalidArgv,
                        message: format!("structured argv command read failed: {e}"),
                    });
                    return JsRuntimeOutput {
                        payload: JsRuntimeOutputPayload::None,
                        diagnostics,
                    };
                }
            };
            let Some(s) = v.as_string() else {
                diagnostics.push(JsDiagnostic {
                    code: JsDiagnosticCode::InvalidArgv,
                    message: "structured argv command is not a string".into(),
                });
                return JsRuntimeOutput {
                    payload: JsRuntimeOutputPayload::None,
                    diagnostics,
                };
            };
            match s.to_string() {
                Ok(s) if !s.is_empty() => argv.push(s),
                Ok(_) => {
                    diagnostics.push(JsDiagnostic {
                        code: JsDiagnosticCode::InvalidArgv,
                        message: "structured argv command must not be empty".into(),
                    });
                    return JsRuntimeOutput {
                        payload: JsRuntimeOutputPayload::None,
                        diagnostics,
                    };
                }
                Err(e) => {
                    diagnostics.push(JsDiagnostic {
                        code: JsDiagnosticCode::InvalidArgv,
                        message: format!("structured argv command decode failed: {e}"),
                    });
                    return JsRuntimeOutput {
                        payload: JsRuntimeOutputPayload::None,
                        diagnostics,
                    };
                }
            }
        }
        let has_args = match obj.contains_key("args") {
            Ok(b) => b,
            Err(e) => {
                diagnostics.push(JsDiagnostic {
                    code: JsDiagnosticCode::InvalidArgv,
                    message: format!("failed to inspect descriptor.args: {e}"),
                });
                return JsRuntimeOutput {
                    payload: JsRuntimeOutputPayload::None,
                    diagnostics,
                };
            }
        };
        if has_args {
            let v: Value<'js> = match obj.get("args") {
                Ok(v) => v,
                Err(e) => {
                    diagnostics.push(JsDiagnostic {
                        code: JsDiagnosticCode::InvalidArgv,
                        message: format!("structured argv args read failed: {e}"),
                    });
                    return JsRuntimeOutput {
                        payload: JsRuntimeOutputPayload::None,
                        diagnostics,
                    };
                }
            };
            let Some(args_arr) = v.as_array() else {
                diagnostics.push(JsDiagnostic {
                    code: JsDiagnosticCode::InvalidArgv,
                    message: "structured argv args is not an array".into(),
                });
                return JsRuntimeOutput {
                    payload: JsRuntimeOutputPayload::None,
                    diagnostics,
                };
            };
            for i in 0..args_arr.len() {
                let elem: Value<'js> = match args_arr.get(i) {
                    Ok(e) => e,
                    Err(e) => {
                        diagnostics.push(JsDiagnostic {
                            code: JsDiagnosticCode::InvalidArgv,
                            message: format!("structured argv args[{i}] read failed: {e}"),
                        });
                        return JsRuntimeOutput {
                            payload: JsRuntimeOutputPayload::None,
                            diagnostics,
                        };
                    }
                };
                let Some(s) = elem.as_string() else {
                    diagnostics.push(JsDiagnostic {
                        code: JsDiagnosticCode::InvalidArgv,
                        message: format!("structured argv args[{i}] is not a string"),
                    });
                    return JsRuntimeOutput {
                        payload: JsRuntimeOutputPayload::None,
                        diagnostics,
                    };
                };
                match s.to_string() {
                    Ok(s) => argv.push(s),
                    Err(e) => {
                        diagnostics.push(JsDiagnostic {
                            code: JsDiagnosticCode::InvalidArgv,
                            message: format!("structured argv args[{i}] decode failed: {e}"),
                        });
                        return JsRuntimeOutput {
                            payload: JsRuntimeOutputPayload::None,
                            diagnostics,
                        };
                    }
                }
            }
        }
        if argv.is_empty() {
            diagnostics.push(JsDiagnostic {
                code: JsDiagnosticCode::InvalidArgv,
                message: "structured argv descriptor produced empty argv".into(),
            });
            return JsRuntimeOutput {
                payload: JsRuntimeOutputPayload::None,
                diagnostics,
            };
        }
        return JsRuntimeOutput {
            payload: JsRuntimeOutputPayload::Argv(argv),
            diagnostics,
        };
    }

    diagnostics.push(JsDiagnostic {
        code: JsDiagnosticCode::InvalidArgv,
        message: "script_function must return an argv array or {command, args}".into(),
    });
    JsRuntimeOutput {
        payload: JsRuntimeOutputPayload::None,
        diagnostics,
    }
}

struct DeadlineGuard<'a> {
    deadline: &'a AtomicI64,
    atomic: i64,
}

impl Drop for DeadlineGuard<'_> {
    fn drop(&mut self) {
        self.deadline.store(self.atomic, Ordering::Relaxed);
    }
}

fn deadline_relative_to(start: Instant, deadline: Instant) -> i64 {
    let nanos = deadline.saturating_duration_since(start).as_nanos();
    nanos.min(i64::MAX as u128) as i64
}

/// Map a caught rquickjs error into the diagnostic the user will see.
fn classify_error(
    err: rquickjs::CaughtError<'_>,
    interrupted: &AtomicBool,
    timeout_ms: u64,
) -> JsRuntimeOutput {
    use rquickjs::CaughtError;

    // The interrupt handler latched first — it's a timeout regardless of
    // the rquickjs-level error type (interrupts surface as exceptions).
    if interrupted.load(Ordering::SeqCst) {
        return JsRuntimeOutput::empty_with(JsDiagnostic {
            code: JsDiagnosticCode::Timeout,
            message: format!("evaluation aborted after ~{timeout_ms}ms wall clock"),
        });
    }

    match err {
        CaughtError::Exception(exc) => {
            let msg = exc.message().unwrap_or_else(|| "<no message>".into());
            let code = exc
                .as_object()
                .get::<_, String>("code")
                .ok()
                .and_then(|code| diagnostic_code_from_host_error(&code))
                .unwrap_or(JsDiagnosticCode::Exception);
            JsRuntimeOutput::empty_with(JsDiagnostic { code, message: msg })
        }
        CaughtError::Value(value) => {
            // Non-Error throws (e.g. `throw "boom"`) come back as a raw value.
            let msg = match value.try_into_string() {
                Ok(s) => s.to_string().unwrap_or_else(|_| "<unstringifiable>".into()),
                Err(_) => "<non-string thrown value>".into(),
            };
            JsRuntimeOutput::empty_with(JsDiagnostic {
                code: JsDiagnosticCode::Exception,
                message: msg,
            })
        }
        CaughtError::Error(rquickjs::Error::Allocation) => {
            JsRuntimeOutput::empty_with(JsDiagnostic {
                code: JsDiagnosticCode::MemoryExceeded,
                message: format!(
                    "QuickJS reported allocation failure (limit {MEMORY_LIMIT_BYTES} bytes)"
                ),
            })
        }
        CaughtError::Error(other) => JsRuntimeOutput::empty_with(JsDiagnostic {
            code: JsDiagnosticCode::Exception,
            message: format!("rquickjs error: {other}"),
        }),
    }
}

fn diagnostic_code_from_host_error(code: &str) -> Option<JsDiagnosticCode> {
    match code {
        "ShellCommandStringDenied" => Some(JsDiagnosticCode::ShellCommandStringDenied),
        "ShellCommandLimitExceeded" => Some(JsDiagnosticCode::ShellCommandLimitExceeded),
        "ShellCommandFailed" => Some(JsDiagnosticCode::ShellCommandFailed),
        "UnsupportedHostApi" => Some(JsDiagnosticCode::UnsupportedHostApi),
        _ => None,
    }
}
