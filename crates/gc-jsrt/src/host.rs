//! Phase 5 host bindings: the minimal Fig-compatible API surface
//! exposed to `script_function` and `custom` JS generators.
//!
//! The bindings are installed into the same fresh per-job [`Ctx`] used
//! by Phase 4. All values are immutable for the duration of evaluation:
//! the JS code reads them, possibly invokes `executeShellCommand`, and
//! returns. There is no mechanism to write back into the host.
//!
//! # Surface
//!
//! ```js
//! const __ghost = {
//!   cwd: "<absolute path>",
//!   env: { HOME: "...", PATH: "...", ... },        // filtered
//!   searchTerm: "<current_token>",                 // alias
//!   currentToken: "<current_token>",
//!   previousToken: "<previous_token>",
//!   tokens: ["git", "checkout", "main"],
//!   executeShellCommand: function(argvOrString) { ... },
//! };
//! ```
//!
//! `__ghost` is also installed as `globalThis.fig` in a backwards-compat
//! shape so the corpus' `fig.executeShellCommand(...)` call sites
//! resolve. Note this is NOT a perfect Fig emulation — the original Fig
//! API surface is large; we expose only what current shipped specs
//! actually need at evaluation time.
//!
//! # Recursion cap
//!
//! `executeShellCommand` is metered by [`MAX_SHELL_CALLS_PER_EVALUATION`].
//! The first five calls succeed/fail per the runner's verdict; the
//! sixth raises a JS exception with diagnostic
//! [`crate::JsDiagnosticCode::ShellCommandLimitExceeded`].
//!
//! # Synchronous semantics
//!
//! JS sees `executeShellCommand` as a synchronous function returning a
//! Fig-compatible result object with `stdout`, `stderr`, and `exitCode`.
//! Spec authors typically do
//! `await executeShellCommand(...)`, which works fine: awaiting a
//! non-Promise returns the value as-is. We don't wrap in a Promise to
//! keep the worker thread (which is a regular OS thread, not a tokio
//! task) able to call `tokio::runtime::Handle::block_on(...)` from
//! inside the runner without worrying about nesting two async
//! interpreters.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rquickjs::function::{Constructor, Rest};
use rquickjs::prelude::Func;
use rquickjs::{Ctx, Function, IntoJs, Object, Value};

use crate::types::{JsRuntimeInput, ShellRunError, ShellRunner};

/// Maximum number of `executeShellCommand` calls a single JS evaluation
/// may make before further calls throw
/// [`crate::JsDiagnosticCode::ShellCommandLimitExceeded`]. Phase 5 picks
/// 5 because every shipped `Custom` generator we audited uses ≤ 2 calls
/// in a single completion turn; 5 covers padding for branches that
/// fan out by configuration step.
pub const MAX_SHELL_CALLS_PER_EVALUATION: usize = 5;

const DEFAULT_SHELL_TIMEOUT_MS: u64 = 5_000;

/// Per-evaluation accounting for `executeShellCommand`. Created fresh
/// in `run_job` and shared (via `Arc`) with the host closure.
#[derive(Default)]
pub(crate) struct HostState {
    /// Number of `executeShellCommand` calls made in this evaluation.
    pub call_count: AtomicUsize,
    /// Whether a host call observed `UnsupportedHostApi` and emitted a
    /// diagnostic. Surfaced through `consume_unsupported_apis`.
    pub unsupported_apis: parking_lot_lite::Mutex<Vec<String>>,
}

impl HostState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Drain the accumulated unsupported-API names for diagnostics.
    pub(crate) fn drain_unsupported(&self) -> Vec<String> {
        let mut guard = self.unsupported_apis.lock();
        std::mem::take(&mut *guard)
    }

    /// Record a missing host API name. Idempotent for the same name in
    /// a single evaluation.
    pub(crate) fn record_unsupported(&self, name: &str) {
        let mut guard = self.unsupported_apis.lock();
        if !guard.iter().any(|n| n == name) {
            guard.push(name.to_string());
        }
    }
}

/// Tiny `parking_lot`-shaped mutex implemented over `std::sync::Mutex`
/// to avoid taking a new dependency. Same trait surface (`lock`)
/// without the poisoning that `std::sync::Mutex` carries.
pub(crate) mod parking_lot_lite {
    use std::cell::UnsafeCell;
    use std::ops::{Deref, DerefMut};
    use std::sync::Mutex as StdMutex;

    /// One-trick mutex returning `MutexGuard` that ignores poisoning.
    pub struct Mutex<T>(StdMutex<UnsafeCell<T>>);

    impl<T> Default for Mutex<T>
    where
        T: Default,
    {
        fn default() -> Self {
            Self(StdMutex::new(UnsafeCell::new(T::default())))
        }
    }

    impl<T> Mutex<T> {
        pub fn lock(&self) -> Guard<'_, T> {
            // Recover from a poisoned mutex by taking the inner state
            // verbatim — Phase 5 host state has no invariants that a
            // panic could leave invalid; the worst case is an empty
            // unsupported-APIs vec and a zero call counter on a dead
            // worker, neither of which is dangerous.
            let lock = match self.0.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            Guard { _lock: lock }
        }
    }

    pub struct Guard<'a, T> {
        _lock: std::sync::MutexGuard<'a, UnsafeCell<T>>,
    }

    impl<T> Deref for Guard<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            // Safety: holding the std MutexGuard means we have unique
            // access to the UnsafeCell contents.
            unsafe { &*self._lock.get() }
        }
    }

    impl<T> DerefMut for Guard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            // Safety: see Deref.
            unsafe { &mut *self._lock.get() }
        }
    }

    // The `UnsafeCell<T>` makes `Mutex<T>` not auto-Sync; re-assert it.
    unsafe impl<T: Send> Send for Mutex<T> {}
    unsafe impl<T: Send> Sync for Mutex<T> {}
}

/// Install the Fig-compatible host API onto `ctx`'s globals.
///
/// `state` is shared with the closure that backs `executeShellCommand`
/// so the recursion cap and unsupported-API accounting persist across
/// every call site within the same evaluation.
pub(crate) fn install_host_api<'js>(
    ctx: &Ctx<'js>,
    input: &JsRuntimeInput,
    state: Arc<HostState>,
    shell_deadline: Instant,
) -> rquickjs::Result<()> {
    let globals: Object<'js> = ctx.globals();

    // ---- Static fields ---------------------------------------------------
    let cwd_str = input.cwd.to_string_lossy().into_owned();
    let search_term = input.current_token.clone();
    let current_token = input.current_token.clone();
    let previous_token = input.previous_token.clone();

    // Build the env object once.
    let env_obj = Object::new(ctx.clone())?;
    for (k, v) in &input.env {
        env_obj.set(k.as_str(), v.as_str())?;
    }

    // Build the tokens array.
    let tokens_arr = rquickjs::Array::new(ctx.clone())?;
    for (idx, tok) in input.tokens.iter().enumerate() {
        tokens_arr.set(idx, tok.as_str())?;
    }

    // ---- executeShellCommand binding ------------------------------------
    let runner = input.shell_runner.clone();
    let allow_shell_command = input.allow_shell_command;
    let exec_fn: Function<'js> = build_execute_shell_command(
        ctx,
        runner,
        allow_shell_command,
        state.clone(),
        input.cwd.clone(),
        shell_deadline,
    )?;

    // ---- Build the canonical __ghost object ------------------------------
    let ghost = Object::new(ctx.clone())?;
    ghost.set("cwd", cwd_str.as_str())?;
    ghost.set("env", env_obj.clone())?;
    ghost.set("searchTerm", search_term.as_str())?;
    ghost.set("currentToken", current_token.as_str())?;
    ghost.set("previousToken", previous_token.as_str())?;
    ghost.set("tokens", tokens_arr.clone())?;
    ghost.set("executeShellCommand", exec_fn.clone())?;

    globals.set("__ghost", ghost)?;

    // ---- Backwards-compat aliases the corpus typically reaches for ------
    // Top-level bindings the Fig API exposes: spec authors write things
    // like `searchTerm`, `currentWorkingDirectory`, `executeShellCommand`
    // directly without going through a `ctx` object. Mirror those into
    // the global scope so we don't force every spec to be rewritten.
    globals.set("currentWorkingDirectory", cwd_str.as_str())?;
    globals.set("environmentVariables", env_obj.clone())?;
    globals.set("searchTerm", search_term.as_str())?;
    globals.set("currentToken", current_token.as_str())?;
    globals.set("previousToken", previous_token.as_str())?;
    globals.set("tokens", tokens_arr.clone())?;
    globals.set("executeShellCommand", exec_fn.clone())?;

    // The `fig` namespace mirrors a subset of the @withfig/api surface.
    // Unknown sub-properties resolve to a thrower so generators that
    // reach for `fig.fs.readFile` produce a stable diagnostic instead
    // of a silent `undefined`.
    let fig = Object::new(ctx.clone())?;
    fig.set("cwd", cwd_str.as_str())?;
    fig.set("env", env_obj.clone())?;
    fig.set("searchTerm", search_term.as_str())?;
    fig.set("currentToken", current_token.as_str())?;
    fig.set("previousToken", previous_token.as_str())?;
    fig.set("tokens", tokens_arr.clone())?;
    fig.set("executeShellCommand", exec_fn)?;
    install_unsupported_namespaces(ctx, &fig, &state)?;
    globals.set("fig", fig)?;

    Ok(())
}

/// Build the `executeShellCommand` closure.
///
/// JS may call this with either a string (`"ls -la"`) or an array
/// (`["ls", "-la"]`). The closure resolves the form, applies the
/// `allow_shell_command` policy, and synchronously delegates to the
/// runner. The thread-bounded counter caps total calls per evaluation
/// at [`MAX_SHELL_CALLS_PER_EVALUATION`].
///
/// Returns a Fig-compatible result object on success; throws on any
/// error path so JS code can branch on `try`/`catch` if desired.
fn build_execute_shell_command<'js>(
    ctx: &Ctx<'js>,
    runner: Option<Arc<dyn ShellRunner>>,
    allow_shell_command: bool,
    state: Arc<HostState>,
    default_cwd: PathBuf,
    shell_deadline: Instant,
) -> rquickjs::Result<Function<'js>> {
    // Default timeout is clamped per call by the remaining JS budget so
    // a native shell run cannot outlive its owning evaluation.

    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> rquickjs::Result<Value<'js>> {
            // Recursion cap.
            let prior = state.call_count.fetch_add(1, Ordering::SeqCst);
            if prior >= MAX_SHELL_CALLS_PER_EVALUATION {
                let msg = format!(
                    "executeShellCommand call cap reached \
                         (limit {MAX_SHELL_CALLS_PER_EVALUATION})"
                );
                return Err(throw_with_code(&ctx, "ShellCommandLimitExceeded", &msg));
            }

            let mut iter = args.0.into_iter();
            let cmd_arg = iter.next().ok_or_else(|| {
                throw_with_code(
                    &ctx,
                    "ShellCommandFailed",
                    "executeShellCommand requires at least one argument",
                )
            })?;

            // Optional second argument: an options object with `cwd`
            // / `timeout` overrides. Phase 5 only honours these two
            // fields; everything else is ignored without a
            // diagnostic so legacy specs that pass extra options
            // (e.g. `splitOn`) keep working.
            let opts_arg = iter.next();
            let (cwd_override, timeout_override) = parse_exec_options(&ctx, opts_arg.as_ref());

            let mut cwd = cwd_override.unwrap_or_else(|| default_cwd.clone());
            let mut timeout_override = timeout_override;

            let Some(ref runner) = runner else {
                return Err(throw_with_code(
                    &ctx,
                    "ShellCommandFailed",
                    "no shell runner installed (test fixture without runner)",
                ));
            };

            let result = if let Some(s) = cmd_arg.as_string() {
                let s = s.to_string()?;
                let timeout = bounded_shell_timeout(timeout_override, shell_deadline);
                if allow_shell_command {
                    runner.run_string(&s, &cwd, timeout)
                } else {
                    return Err(throw_with_code(
                        &ctx,
                        "ShellCommandStringDenied",
                        &format!(
                            "shell-string form denied (allow_shell_command=false): \
                                 {s:?}"
                        ),
                    ));
                }
            } else if let Some(arr) = cmd_arg.as_array() {
                let mut argv: Vec<String> = Vec::with_capacity(arr.len());
                for i in 0..arr.len() {
                    let v: Value<'js> = arr.get(i)?;
                    let Some(s) = v.as_string() else {
                        return Err(throw_with_code(
                            &ctx,
                            "ShellCommandFailed",
                            &format!("argv element [{i}] is not a string"),
                        ));
                    };
                    argv.push(s.to_string()?);
                }
                if argv.is_empty() {
                    return Err(throw_with_code(
                        &ctx,
                        "ShellCommandFailed",
                        "argv array must have at least one element",
                    ));
                }
                let timeout = bounded_shell_timeout(timeout_override, shell_deadline);
                runner.run_argv(&argv, &cwd, timeout)
            } else if let Some(obj) = cmd_arg.as_object() {
                // Fig also supports `{ command: "...", args: [...] }`
                // descriptors. We accept both `command` (single
                // executable name) and `args` (rest of argv).
                let (descriptor_cwd, descriptor_timeout) = parse_exec_options(&ctx, Some(&cmd_arg));
                if let Some(descriptor_cwd) = descriptor_cwd {
                    cwd = descriptor_cwd;
                }
                if descriptor_timeout.is_some() {
                    timeout_override = descriptor_timeout;
                }
                let mut argv: Vec<String> = Vec::new();
                let command_value: Value<'js> = obj.get("command").map_err(|_| {
                    throw_with_code(
                        &ctx,
                        "ShellCommandFailed",
                        "structured command descriptor requires command",
                    )
                })?;
                let Some(command) = command_value.as_string() else {
                    return Err(throw_with_code(
                        &ctx,
                        "ShellCommandFailed",
                        "structured command descriptor command is not a string",
                    ));
                };
                let command = command.to_string()?;
                if command.is_empty() {
                    return Err(throw_with_code(
                        &ctx,
                        "ShellCommandFailed",
                        "structured command descriptor command must not be empty",
                    ));
                }
                argv.push(command);
                if obj.contains_key("args").unwrap_or(false) {
                    let v: Value<'js> = obj.get("args")?;
                    let Some(args_arr) = v.as_array() else {
                        return Err(throw_with_code(
                            &ctx,
                            "ShellCommandFailed",
                            "structured command descriptor args is not an array",
                        ));
                    };
                    for i in 0..args_arr.len() {
                        let elem: Value<'js> = args_arr.get(i)?;
                        let Some(s) = elem.as_string() else {
                            return Err(throw_with_code(
                                &ctx,
                                "ShellCommandFailed",
                                &format!("structured command descriptor args[{i}] is not a string"),
                            ));
                        };
                        argv.push(s.to_string()?);
                    }
                }
                let timeout = bounded_shell_timeout(timeout_override, shell_deadline);
                runner.run_argv(&argv, &cwd, timeout)
            } else {
                return Err(throw_with_code(
                    &ctx,
                    "ShellCommandFailed",
                    "executeShellCommand: argument must be a string, array, or {command, args}",
                ));
            };

            let output = match result {
                Ok(o) => o,
                Err(ShellRunError::StringDenied) => {
                    return Err(throw_with_code(
                        &ctx,
                        "ShellCommandStringDenied",
                        "runner denied shell-string command",
                    ));
                }
                Err(other) => {
                    return Err(throw_with_code(
                        &ctx,
                        "ShellCommandFailed",
                        &format!("{other}"),
                    ));
                }
            };

            let result_obj = Object::new(ctx.clone())?;
            result_obj.set("stdout", output.stdout.as_str())?;
            result_obj.set("stderr", output.stderr.as_str())?;
            result_obj.set("exitCode", output.exit_code)?;
            Ok(result_obj.into_value())
        },
    )
}

fn bounded_shell_timeout(requested_timeout_ms: Option<u64>, deadline: Instant) -> Duration {
    let requested = Duration::from_millis(requested_timeout_ms.unwrap_or(DEFAULT_SHELL_TIMEOUT_MS));
    let remaining = deadline.saturating_duration_since(Instant::now());
    requested.min(remaining)
}

/// Parse the optional options descriptor that JS may pass as the second
/// argument to `executeShellCommand`. Honoured fields:
///
///   * `cwd: string`  – override the default cwd for this call.
///   * `timeout: number` – override the default timeout (milliseconds).
///
/// Unknown fields are silently ignored.
fn parse_exec_options<'js>(
    _ctx: &Ctx<'js>,
    opts: Option<&Value<'js>>,
) -> (Option<std::path::PathBuf>, Option<u64>) {
    let Some(opts) = opts else {
        return (None, None);
    };
    let Some(obj) = opts.as_object() else {
        return (None, None);
    };
    let cwd = obj.get::<_, Value<'js>>("cwd").ok().and_then(|v| {
        v.as_string()
            .and_then(|s| s.to_string().ok())
            .map(std::path::PathBuf::from)
    });
    let timeout = obj
        .get::<_, Value<'js>>("timeout")
        .ok()
        .and_then(|v| v.as_number().map(|n| n as u64));
    (cwd, timeout)
}

/// Throw a JS Error with a stable diagnostic code attached.
///
/// We tag the error via a custom property `code` so the runtime's
/// classification path can pick it up alongside the message. The Fig
/// API itself does not have stable codes; ours is an additive
/// extension specific to gc-jsrt.
fn throw_with_code<'js>(ctx: &Ctx<'js>, code: &str, message: &str) -> rquickjs::Error {
    let exc = match build_error_object(ctx, code, message) {
        Ok(v) => v,
        Err(_) => {
            // If we can't build the structured error, fall back to a
            // raw string throw so the worker still classifies as
            // Exception with the message text.
            let fallback = match message.into_js(ctx) {
                Ok(v) => v,
                Err(_) => Value::new_undefined(ctx.clone()),
            };
            return ctx.throw(fallback);
        }
    };
    ctx.throw(exc)
}

fn build_error_object<'js>(
    ctx: &Ctx<'js>,
    code: &str,
    message: &str,
) -> rquickjs::Result<Value<'js>> {
    // Construct via `new Error(message)` so the prototype chain matches
    // user expectations; then attach a `.code` property.
    let globals = ctx.globals();
    let error_ctor: Constructor<'js> = globals.get("Error")?;
    let err: Value<'js> = error_ctor.construct((message,))?;
    if let Some(obj) = err.as_object() {
        obj.set("code", code)?;
    }
    Ok(err)
}

/// Install thrower stubs for namespaces that the corpus references but
/// the Phase 5 runtime cannot fulfil (e.g. `fig.fs.readFile`,
/// `fig.path.join`). Each unknown call is reported via `state.record_unsupported`
/// so the engine can emit a diagnostic.
fn install_unsupported_namespaces<'js>(
    ctx: &Ctx<'js>,
    fig: &Object<'js>,
    state: &Arc<HostState>,
) -> rquickjs::Result<()> {
    // Top-level namespaces we lower with throwers. Adding `fs`, `path`,
    // and `keychain` covers the dominant API names the inventory shows
    // outside of executeShellCommand.
    for ns_name in ["fs", "path", "keychain", "ipc", "ui"] {
        let namespace = Object::new(ctx.clone())?;
        for method in ["readFile", "writeFile", "stat", "exists", "join", "resolve"] {
            let label = format!("fig.{}.{}", ns_name, method);
            let state_for_method = state.clone();
            let method_label = label.clone();
            namespace.set(
                method,
                Func::new(move |ctx: Ctx<'js>| -> rquickjs::Result<Value<'js>> {
                    state_for_method.record_unsupported(&method_label);
                    Err(throw_with_code(
                        &ctx,
                        "UnsupportedHostApi",
                        &format!("{method_label} is not implemented in gc-jsrt"),
                    ))
                }),
            )?;
        }
        fig.set(ns_name, namespace)?;
    }
    Ok(())
}
