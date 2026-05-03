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

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
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
    JsRuntimeOutput,
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
    /// Caller-provided context. Phase 4 only uses `stdout`; Phase 5
    /// reads `tokens` / `cwd` / `env` / `current_token` / `previous_token`
    /// to populate the host bindings, and the `kind` discriminator
    /// chooses the dispatch shape.
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
        // Closing the channel implicitly happens when `JsWorker.sender`
        // is dropped — but `JsWorker` is `Clone` so we can't take it
        // here. Instead the worker loop exits when it observes the
        // channel disconnect; we just join.
        //
        // TODO(ux-9 follow-up): `thread.join()` blocks until the current
        // JS evaluation finishes (capped only by the per-job interrupt
        // deadline, which can still be seconds). A long-running custom
        // generator can therefore stall process shutdown. Replace with a
        // bounded shutdown — e.g. a stop-signal channel that the worker
        // loop polls between jobs, plus a `JoinHandle::is_finished` +
        // ~2s ceiling so a slow evaluator never blocks `Drop` longer than
        // the wall-clock budget already promises. Tracked as a follow-up
        // post-UX-9.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
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
    let interrupted_flag = Arc::new(AtomicU64::new(0));

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
                interrupted.store(1, Ordering::SeqCst);
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
    interrupted: &AtomicU64,
    process_start: Instant,
) -> Result<JsRuntimeOutput, JsRuntimeError> {
    // Reset the timeout latch and arm the interrupt handler.
    interrupted.store(0, Ordering::SeqCst);
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

        // Phase 5: install host bindings for ScriptFunction / Custom
        // jobs. PostProcess jobs do not need them — but installing
        // unconditionally is cheap (a few property sets on a fresh
        // context) and gives spec authors a consistent surface.
        if let Err(e) = install_host_api(&ctx, &job.input, host_state.clone()) {
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
/// Anything else surfaces as `InvalidArgv`. The output's
/// `argv` field is populated in place; the suggestions vec stays empty
/// because `script_function` produces no suggestions of its own — those
/// come from the follow-up script invocation in the engine.
fn normalize_argv<'js>(_ctx: &rquickjs::Ctx<'js>, value: Value<'js>) -> JsRuntimeOutput {
    let mut output = JsRuntimeOutput::default();

    // Accept both `["cmd", "arg"]` and `{ command, args }`.
    if let Some(arr) = value.as_array() {
        let mut argv: Vec<String> = Vec::with_capacity(arr.len());
        for i in 0..arr.len() {
            let v: Value<'js> = match arr.get(i) {
                Ok(v) => v,
                Err(e) => {
                    output.diagnostics.push(JsDiagnostic {
                        code: JsDiagnosticCode::InvalidArgv,
                        message: format!("argv element [{i}] read failed: {e}"),
                    });
                    return output;
                }
            };
            let Some(s) = v.as_string() else {
                output.diagnostics.push(JsDiagnostic {
                    code: JsDiagnosticCode::InvalidArgv,
                    message: format!("argv element [{i}] is not a string"),
                });
                return output;
            };
            match s.to_string() {
                Ok(s) => argv.push(s),
                Err(e) => {
                    output.diagnostics.push(JsDiagnostic {
                        code: JsDiagnosticCode::InvalidArgv,
                        message: format!("argv element [{i}] decode failed: {e}"),
                    });
                    return output;
                }
            }
        }
        if argv.is_empty() {
            output.diagnostics.push(JsDiagnostic {
                code: JsDiagnosticCode::InvalidArgv,
                message: "argv array must have at least one element".into(),
            });
            return output;
        }
        output.argv = argv;
        return output;
    }

    if let Some(obj) = value.as_object() {
        let executable: String = match obj.get::<_, Value<'js>>("command") {
            Ok(v) => match v.as_string() {
                Some(s) => s.to_string().unwrap_or_default(),
                None => String::new(),
            },
            Err(_) => String::new(),
        };
        let mut argv: Vec<String> = Vec::new();
        if !executable.is_empty() {
            argv.push(executable);
        }
        if let Ok(v) = obj.get::<_, Value<'js>>("args") {
            if let Some(args_arr) = v.as_array() {
                for i in 0..args_arr.len() {
                    let elem: Value<'js> = match args_arr.get(i) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    if let Some(s) = elem.as_string() {
                        if let Ok(s) = s.to_string() {
                            argv.push(s);
                        }
                    }
                }
            }
        }
        if argv.is_empty() {
            output.diagnostics.push(JsDiagnostic {
                code: JsDiagnosticCode::InvalidArgv,
                message: "structured argv descriptor produced empty argv".into(),
            });
            return output;
        }
        output.argv = argv;
        return output;
    }

    output.diagnostics.push(JsDiagnostic {
        code: JsDiagnosticCode::InvalidArgv,
        message: "script_function must return an argv array or {command, args}".into(),
    });
    output
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
    interrupted: &AtomicU64,
    timeout_ms: u64,
) -> JsRuntimeOutput {
    use rquickjs::CaughtError;

    // The interrupt handler latched first — it's a timeout regardless of
    // the rquickjs-level error type (interrupts surface as exceptions).
    if interrupted.load(Ordering::SeqCst) != 0 {
        return JsRuntimeOutput::empty_with(JsDiagnostic {
            code: JsDiagnosticCode::Timeout,
            message: format!("evaluation aborted after ~{timeout_ms}ms wall clock"),
        });
    }

    match err {
        CaughtError::Exception(exc) => {
            let msg = exc.message().unwrap_or_else(|| "<no message>".into());
            JsRuntimeOutput::empty_with(JsDiagnostic {
                code: JsDiagnosticCode::Exception,
                message: msg,
            })
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
