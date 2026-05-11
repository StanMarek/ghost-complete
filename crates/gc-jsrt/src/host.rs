//! Minimal Fig-compatible host bindings exposed to `script_function`
//! and `custom` JS generators.
//!
//! The bindings install into the per-job [`Ctx`] and are immutable for
//! the duration of evaluation. JS reads them, possibly invokes
//! `executeShellCommand`, and returns; there is no write-back path.
//!
//! # Surface
//!
//! ```js
//! const __ghost = {
//!   cwd: "<absolute path>",
//!   env: { HOME: "...", PATH: "...", ... },
//!   searchTerm: "<current_token>",
//!   currentToken: "<current_token>",
//!   previousToken: "<previous_token>",
//!   tokens: ["git", "checkout", "main"],
//!   executeShellCommand: function(argvOrString) { ... },
//! };
//! ```
//!
//! `__ghost` is also installed as `globalThis.fig` in a backwards-compat
//! shape so corpus call sites of the form `fig.executeShellCommand(...)`
//! resolve. This is NOT a perfect Fig emulation — only the bits current
//! shipped specs actually need.
//!
//! `executeShellCommand` is metered by [`MAX_SHELL_CALLS_PER_EVALUATION`];
//! the next call after that throws
//! [`crate::JsDiagnosticCode::ShellCommandLimitExceeded`].
//!
//! JS sees `executeShellCommand` as synchronous (returning a Fig-shape
//! result with `stdout` / `stderr` / `exitCode`). `await` on a non-Promise
//! returns the value as-is, so the corpus' `await ...` style works.
//! We avoid wrapping in a Promise so the worker thread (a regular OS
//! thread, not a tokio task) can `Handle::block_on(...)` inside the runner
//! without nesting two async interpreters.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rquickjs::function::{Constructor, Rest};
use rquickjs::prelude::Func;
use rquickjs::{CatchResultExt, Ctx, Function, IntoJs, Object, Value};

use crate::types::{JsRuntimeInput, ShellRunError, ShellRunner};

/// Fig-compatible single-letter helper definitions (`l`, `p`, `c`, `d`,
/// `h`, `f`) installed into every freshly-created context before user
/// code runs. See `helpers.js` for provenance and semantics.
///
/// Embedded at compile time so the binary remains self-contained.
const FIG_HELPERS_JS: &str = include_str!("helpers.js");

/// Maximum number of `executeShellCommand` calls a single JS evaluation
/// may make before further calls throw
/// [`crate::JsDiagnosticCode::ShellCommandLimitExceeded`]. Bounded at 5
/// because every shipped `Custom` generator uses ≤ 2 calls per turn;
/// 5 leaves headroom for fan-out without enabling abuse.
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
    pub unsupported_apis: Mutex<Vec<String>>,
}

impl HostState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Drain the accumulated unsupported-API names for diagnostics.
    pub(crate) fn drain_unsupported(&self) -> Vec<String> {
        let mut guard = self
            .unsupported_apis
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *guard)
    }

    /// Record a missing host API name. Idempotent for the same name in
    /// a single evaluation.
    pub(crate) fn record_unsupported(&self, name: &str) {
        let mut guard = self
            .unsupported_apis
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !guard.iter().any(|n| n == name) {
            guard.push(name.to_string());
        }
    }
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
    // ---- Fig helper preamble --------------------------------------------
    // Bind the single-letter helpers (`l`, `p`, `c`, `d`, `h`, `f`) before
    // exposing __ghost so user-supplied post_process bodies resolve them.
    // The preamble is pure JSON-manipulation JS — it cannot escape the
    // sandbox even if a future helper becomes buggy, because no host
    // bindings have been installed yet.
    install_fig_helpers(ctx)?;

    let globals: Object<'js> = ctx.globals();

    // ---- Static fields ---------------------------------------------------
    let cwd_str = input.cwd.to_string_lossy().into_owned();
    let search_term = input.current_token.clone();
    let current_token = input.current_token.clone();
    let previous_token = input.previous_token.clone();

    let env_obj = Object::new(ctx.clone())?;
    for (k, v) in &input.env {
        env_obj.set(k.as_str(), v.as_str())?;
    }

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
    // Fig spec authors write `searchTerm` / `currentWorkingDirectory` /
    // `executeShellCommand` directly without going through any context
    // object, so we mirror them into the global scope to avoid rewriting
    // every spec.
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

/// Evaluate the Fig helper preamble against `ctx`'s globals.
///
/// The preamble is embedded at compile time via `include_str!` so it
/// rides with the binary. Each freshly-created [`rquickjs::Context`]
/// re-evaluates it because the per-job context is otherwise empty;
/// see `worker.rs::run_job` for the per-job lifecycle.
///
/// A failure here is propagated as `rquickjs::Result::Err` so the
/// caller's `Ok(...)` envelope in `run_job` converts it into an
/// `Exception` diagnostic; user code cannot observe a partial install.
pub(crate) fn install_fig_helpers<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<()> {
    ctx.eval::<(), _>(FIG_HELPERS_JS).catch(ctx).map_err(|e| {
        tracing::error!(error = ?e, "fig helper preamble failed to evaluate");
        rquickjs::Error::Exception
    })
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

            // Unknown options fields are silently ignored so legacy specs
            // that pass extras (e.g. `splitOn`) keep working.
            let opts_arg = iter.next();
            let (cwd_override, timeout_override) =
                parse_exec_options(&ctx, opts_arg.as_ref(), &state);

            // Retain whether the explicit `opts` argument supplied a
            // cwd/timeout — this drives precedence below when the
            // command itself is a structured `{ command, cwd, ... }`
            // descriptor that ALSO carries cwd/timeout fields.
            let cwd_override_was_explicit = cwd_override.is_some();
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
                if !allow_shell_command {
                    return Err(throw_with_code(
                        &ctx,
                        "ShellCommandStringDenied",
                        &format!(
                            "shell-string form denied (allow_shell_command=false): \
                                 {s:?}"
                        ),
                    ));
                }
                match bounded_shell_timeout(timeout_override, shell_deadline) {
                    Some(timeout) => runner.run_string(&s, &cwd, timeout),
                    None => Err(ShellRunError::Timeout),
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
                match bounded_shell_timeout(timeout_override, shell_deadline) {
                    Some(timeout) => runner.run_argv(&argv, &cwd, timeout),
                    None => Err(ShellRunError::Timeout),
                }
            } else if let Some(obj) = cmd_arg.as_object() {
                // Fig also supports `{ command: "...", args: [...] }`
                // descriptors. We accept both `command` (single
                // executable name) and `args` (rest of argv).
                //
                // Precedence: an explicit second `opts` argument wins
                // over a descriptor-embedded `cwd`/`timeout`. Specs that
                // pass both forms simultaneously are rare, but the
                // standard `executeShellCommand(cmd, opts)` shape says
                // the more-specific opts arg overrides anything baked
                // into the command descriptor.
                let (descriptor_cwd, descriptor_timeout) =
                    parse_exec_options(&ctx, Some(&cmd_arg), &state);
                if !cwd_override_was_explicit {
                    if let Some(descriptor_cwd) = descriptor_cwd {
                        cwd = descriptor_cwd;
                    }
                }
                if timeout_override.is_none() && descriptor_timeout.is_some() {
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
                let has_args = match obj.contains_key("args") {
                    Ok(b) => b,
                    Err(e) => {
                        return Err(throw_with_code(
                            &ctx,
                            "ShellCommandFailed",
                            &format!("failed to inspect descriptor.args: {e}"),
                        ));
                    }
                };
                if has_args {
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
                match bounded_shell_timeout(timeout_override, shell_deadline) {
                    Some(timeout) => runner.run_argv(&argv, &cwd, timeout),
                    None => Err(ShellRunError::Timeout),
                }
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

/// Below this threshold the runner is bypassed entirely — `tokio::time::timeout`
/// would still fork+reap a subprocess before resolving Elapsed, paying ~5–15ms
/// on macOS for nothing.
const SHELL_TIMEOUT_FLOOR: Duration = Duration::from_millis(5);

fn bounded_shell_timeout(requested_timeout_ms: Option<u64>, deadline: Instant) -> Option<Duration> {
    let requested = Duration::from_millis(requested_timeout_ms.unwrap_or(DEFAULT_SHELL_TIMEOUT_MS));
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining < SHELL_TIMEOUT_FLOOR {
        return None;
    }
    Some(requested.min(remaining))
}

/// Parse the optional options descriptor that JS may pass as the second
/// argument to `executeShellCommand`. Honoured fields:
///
///   * `cwd: string`  – override the default cwd for this call.
///   * `timeout: number` – override the default timeout (milliseconds).
///
/// Unknown fields are silently ignored so legacy specs that pass extras
/// (e.g. `splitOn`) keep working. Wrong-typed / out-of-range values for
/// known fields emit an `UnsupportedHostApi` diagnostic so a spec
/// author hunting "why is my timeout not honored" gets a signal. The
/// SAME diagnostic shape is also emitted when the entire opts arg is
/// the wrong shape (e.g. a positional number rather than `{ timeout: n }`).
fn parse_exec_options<'js>(
    _ctx: &Ctx<'js>,
    opts: Option<&Value<'js>>,
    state: &Arc<HostState>,
) -> (Option<std::path::PathBuf>, Option<u64>) {
    let Some(opts) = opts else {
        return (None, None);
    };
    // Treat an explicit `undefined`/`null` opts the same as a missing
    // arg: both mean "no overrides". Any other non-object value is a
    // misuse worth surfacing — the typical mistake is
    // `executeShellCommand([...], 5000)` (positional timeout) which
    // would otherwise silently fall back to defaults.
    if opts.is_undefined() || opts.is_null() {
        return (None, None);
    }
    let Some(obj) = opts.as_object() else {
        state.record_unsupported("executeShellCommand.options<not-object>");
        return (None, None);
    };
    let cwd = match obj.get::<_, Value<'js>>("cwd") {
        Ok(v) if !v.is_undefined() && !v.is_null() => match v.as_string() {
            Some(s) => match s.to_string() {
                Ok(s) => Some(std::path::PathBuf::from(s)),
                Err(_) => {
                    // QuickJS produced a string we couldn't decode to
                    // UTF-8 — surface the failure so the spec author
                    // doesn't get a silent fallback to the default cwd.
                    state.record_unsupported("executeShellCommand.options.cwd<decode-failure>");
                    None
                }
            },
            None => {
                state.record_unsupported("executeShellCommand.options.cwd<bad-type>");
                None
            }
        },
        _ => None,
    };
    let timeout = match obj.get::<_, Value<'js>>("timeout") {
        Ok(v) if !v.is_undefined() && !v.is_null() => match v.as_number() {
            Some(n) => {
                // `f64 as u64` is saturating in Rust, but the SEMANTIC
                // wrap (NaN → 0, negative → 0, Infinity → u64::MAX) is
                // surprising. Emit a diagnostic and skip the override
                // so the spec author sees the misuse instead of a
                // silent 0ms or 18-quintillion-ms timeout.
                if !n.is_finite() || n < 0.0 || n > u64::MAX as f64 {
                    state.record_unsupported("executeShellCommand.options.timeout<out-of-range>");
                    None
                } else {
                    Some(n as u64)
                }
            }
            None => {
                state.record_unsupported("executeShellCommand.options.timeout<bad-type>");
                None
            }
        },
        _ => None,
    };
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
            // Fall back to a raw string throw so the worker still
            // classifies as Exception with the message text.
            let fallback = match message.into_js(ctx) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(
                        code = %code,
                        message = %message,
                        error = ?e,
                        "throw_with_code: could not construct any JS exception payload"
                    );
                    Value::new_undefined(ctx.clone())
                }
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

/// Install thrower stubs for namespaces the corpus references but the
/// runtime cannot fulfil (e.g. `fig.fs.readFile`, `fig.path.join`).
/// Each unknown call is reported via `state.record_unsupported` so the
/// engine can emit a diagnostic.
fn install_unsupported_namespaces<'js>(
    ctx: &Ctx<'js>,
    fig: &Object<'js>,
    state: &Arc<HostState>,
) -> rquickjs::Result<()> {
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
