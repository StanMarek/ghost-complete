//! Adapter that wires [`gc_jsrt::JsWorker`] into the suggestion engine.
//!
//! This module sits between `engine.rs` (which dispatches generator results)
//! and `gc_jsrt` (which evaluates JS in a sandboxed worker). It owns three
//! responsibilities:
//!
//! 1. **Lazy worker spawn.** Spawning the worker thread costs ~5 MB and a
//!    QuickJS runtime initialisation; we don't want that price unless a
//!    `requires_js` generator actually fires. [`JsRuntimeAdapter::worker`]
//!    pays it once and memoises the result.
//! 2. **Program assembly.** `js_runtime.source` ships the *body* of the JS
//!    function (e.g. `out => out.split('\n').map(name => ({ name }))`). The
//!    runtime evaluates a top-level expression, so we synthesise a
//!    self-invoking call wrapping the body. The shape of the synthesised
//!    program differs by [`gc_jsrt::JsExecutionKind`] — see
//!    `build_post_process_program`, `build_script_function_program`,
//!    `build_custom_program`, and `build_token_only_program`.
//! 3. **Diagnostic logging.** Every [`gc_jsrt::JsDiagnostic`] is mapped to a
//!    structured `tracing` event so the doctor / status surfaces can render
//!    them without re-implementing the rendering.
//!
//! The adapter is intentionally narrow — no caching, no transform pipeline,
//! no source-hashing. Those live in the engine; we only run JS.
//!
//! Four public methods route on [`gc_jsrt::JsExecutionKind`]:
//! [`JsRuntimeAdapter::post_process`] (script stdout in, suggestions out),
//! [`JsRuntimeAdapter::script_function`] (returns argv for an engine-side
//! script invocation), [`JsRuntimeAdapter::custom`] (returns suggestions
//! directly via host-API calls), and [`JsRuntimeAdapter::token_only`]
//! (same suggestion output shape as `custom`, but asks gc-jsrt to install
//! only token globals — no host APIs).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use gc_jsrt::{
    JsDiagnosticCode, JsExecutionKind, JsRuntimeError, JsRuntimeInput, JsRuntimeOutput, JsWorker,
    ShellRunner,
};

/// Lazily-spawned wrapper around [`gc_jsrt::JsWorker`].
///
/// `Default` is cheap and infallible; the OS thread is only started when
/// [`JsRuntimeAdapter::post_process`] is first called. If the worker fails to
/// spawn we log once at error level and return `Err(WorkerSpawnFailed)` to
/// the caller so it can fall through to the existing transform path or skip
/// the generator entirely.
#[derive(Default)]
pub struct JsRuntimeAdapter {
    worker: OnceLock<JsWorker>,
    /// Serialises concurrent spawn attempts so only one thread pays the
    /// JsWorker spawn cost on first use; see [`JsRuntimeAdapter::worker`].
    spawn_lock: Mutex<()>,
}

impl std::fmt::Debug for JsRuntimeAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsRuntimeAdapter")
            .field("worker_initialised", &self.worker.get().is_some())
            .finish()
    }
}

impl JsRuntimeAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the underlying worker, spawning it on first call. Subsequent
    /// callers get the same worker handle. Errors from the initial spawn are
    /// not memoised — a later caller may successfully spawn even if an
    /// earlier attempt failed (e.g. transient EAGAIN from `pthread_create`).
    ///
    /// We serialise concurrent spawn attempts behind a [`Mutex`]: spawning
    /// a [`JsWorker`] costs an OS thread + a 5 MB QuickJS runtime, and the
    /// loser of a race would synchronously join its discarded worker on
    /// drop. Holding the mutex around the get_or_try_init equivalent
    /// guarantees only one spawn attempt is in flight at a time.
    fn worker(&self) -> Result<&JsWorker, JsRuntimeError> {
        self.worker_with(JsWorker::spawn)
    }

    /// Inner spawn-or-fetch helper, parameterised on the spawn function so
    /// the concurrency test can substitute an instrumented spawner that
    /// counts invocations.
    fn worker_with<F>(&self, spawn: F) -> Result<&JsWorker, JsRuntimeError>
    where
        F: FnOnce() -> Result<JsWorker, JsRuntimeError>,
    {
        if let Some(w) = self.worker.get() {
            return Ok(w);
        }
        let _guard = self.spawn_lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(w) = self.worker.get() {
            return Ok(w);
        }
        let spawned = spawn()?;
        self.worker
            .set(spawned)
            .map_err(|_| JsRuntimeError::Internal("OnceLock unexpectedly populated".into()))?;
        Ok(self.worker.get().expect("OnceLock populated above"))
    }

    /// Run a `post_process` JS source over `stdout`. Returns the normalised
    /// runtime output (suggestions + any diagnostics). Errors only surface
    /// for unrecoverable conditions (worker thread dead, spawn failure);
    /// soft conditions (timeout, exception, oversized output) are returned
    /// as diagnostics on a successful [`JsRuntimeOutput`] with an empty
    /// `suggestions` vec.
    pub async fn post_process(
        &self,
        source: &str,
        stdout: String,
        timeout: Duration,
        generator_id: String,
    ) -> Result<JsRuntimeOutput, JsRuntimeError> {
        let worker = self.worker()?;
        let program = build_post_process_program(source, &stdout);
        let input = JsRuntimeInput {
            stdout: Some(stdout),
            generator_id: generator_id.clone(),
            kind: JsExecutionKind::PostProcess,
            ..JsRuntimeInput::default()
        };
        let output = worker.evaluate(program, input, timeout).await?;
        log_diagnostics(&generator_id, &output);
        Ok(output)
    }

    /// Run a `script_function` JS source. The body is invoked with
    /// `(tokens, ctx)` and is expected to return either an argv array
    /// (`["cmd", "arg"]`) or a structured descriptor
    /// (`{ command: "cmd", args: ["arg"] }`). The caller (engine) is
    /// responsible for spawning the resulting argv as a script.
    ///
    /// `JsRuntimeOutput.argv` carries the resolved argv on success;
    /// `suggestions` is always empty for this shape.
    pub async fn script_function(
        &self,
        source: &str,
        ctx: JsExecContext,
        timeout: Duration,
        generator_id: String,
    ) -> Result<JsRuntimeOutput, JsRuntimeError> {
        let worker = self.worker()?;
        let program = build_script_function_program(source);
        let input = ctx.into_runtime_input(generator_id.clone(), JsExecutionKind::ScriptFunction);
        let output = worker.evaluate(program, input, timeout).await?;
        log_diagnostics(&generator_id, &output);
        Ok(output)
    }

    /// Run a `custom` JS source. The body is invoked with
    /// `(tokens, executeShellCommand, ctx)` and is expected to return
    /// suggestions (or a Promise resolving to suggestions). `executeShellCommand`
    /// is wired to the supplied `ShellRunner`; up to
    /// [`gc_jsrt::MAX_SHELL_CALLS_PER_EVALUATION`] calls are honoured.
    pub async fn custom(
        &self,
        source: &str,
        ctx: JsExecContext,
        timeout: Duration,
        generator_id: String,
        runner: Arc<dyn ShellRunner>,
        allow_shell_command: bool,
    ) -> Result<JsRuntimeOutput, JsRuntimeError> {
        let worker = self.worker()?;
        let program = build_custom_program(source);
        let mut input = ctx.into_runtime_input(generator_id.clone(), JsExecutionKind::Custom);
        input.shell_runner = Some(runner);
        input.allow_shell_command = allow_shell_command;
        let output = worker.evaluate(program, input, timeout).await?;
        log_diagnostics(&generator_id, &output);
        Ok(output)
    }

    /// Run a `token_only` JS source. The body may be either a function
    /// accepting `(tokens, ctx)` or a direct expression that returns
    /// suggestions. The runtime installs only `tokens`, `currentToken`, and
    /// `previousToken`; no host API or Fig namespace is available.
    pub async fn token_only(
        &self,
        source: &str,
        ctx: JsExecContext,
        timeout: Duration,
        generator_id: String,
    ) -> Result<JsRuntimeOutput, JsRuntimeError> {
        let worker = self.worker()?;
        let program = build_token_only_program(source);
        let input = ctx.into_runtime_input(generator_id.clone(), JsExecutionKind::TokenOnly);
        let output = worker.evaluate(program, input, timeout).await?;
        log_diagnostics(&generator_id, &output);
        Ok(output)
    }
}

/// Caller-side shape that captures the host context the engine wants to
/// expose to a JS generator. The adapter copies the contents into the
/// final [`JsRuntimeInput`] without inspecting them.
#[derive(Debug, Clone, Default)]
pub struct JsExecContext {
    pub tokens: Vec<String>,
    pub current_token: String,
    pub previous_token: String,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
}

impl JsExecContext {
    fn into_runtime_input(self, generator_id: String, kind: JsExecutionKind) -> JsRuntimeInput {
        // TokenOnly is contractually denied host cwd/env: the worker installs
        // no host bindings (see `install_token_only_globals` in
        // gc-jsrt/host.rs), so any cwd/env we forwarded would be dead weight
        // at best and a leak vector at worst. Clear them here so the
        // invariant "TokenOnly never receives host cwd/env" is enforced at
        // the data boundary the worker thread observes — a future copy-paste
        // that wires env reads into TokenOnly cannot silently exfiltrate
        // HOME/PATH/AWS_ACCESS_KEY_ID/GITHUB_TOKEN from a host-untrusted
        // sandbox.
        let (cwd, env) = if matches!(kind, JsExecutionKind::TokenOnly) {
            (PathBuf::new(), BTreeMap::new())
        } else {
            (self.cwd, self.env)
        };
        JsRuntimeInput {
            stdout: None,
            tokens: self.tokens,
            current_token: self.current_token,
            previous_token: self.previous_token,
            cwd,
            env,
            generator_id,
            kind,
            allow_shell_command: false,
            shell_runner: None,
        }
    }
}

/// Construct the wrapper expression that invokes the generator body with
/// the script's stdout. The stdout bytes are embedded as a JSON-encoded
/// string literal that doubles as a valid JS string token; the encoded
/// form is spliced directly into the program text without a runtime
/// `JSON.parse` call. See [`json_string_literal`] for the encoding
/// invariant the splice relies on.
fn build_post_process_program(source: &str, stdout: &str) -> String {
    let stdout_literal = json_string_literal(stdout);
    format!(
        "(({source})({stdout_literal}))",
        source = source,
        stdout_literal = stdout_literal,
    )
}

/// Encode a Rust string as a JSON string literal that is also a safe JS
/// string token to splice into program text.
///
/// `serde_json` emits a valid JSON string token. QuickJS targets ES2020,
/// which accepts JSON string syntax verbatim (including raw U+2028 /
/// U+2029, which serde_json emits unescaped — these are line/paragraph
/// separators that were illegal in JS string literals before ES2019). We
/// splice the encoded form directly without an extra `JSON.parse` round
/// trip.
fn json_string_literal(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// Construct the wrapper expression for a `script_function` generator.
///
/// The JS source is a function (typically arrow-form) that takes
/// `(tokens, ctx)` and returns either an argv array or a
/// `{command, args}` descriptor. Host bindings (cwd / env / etc.) are
/// available as both top-level globals AND inside `ctx`. We pass the
/// `tokens` array explicitly so generators that destructure
/// `[, cmd, sub] = tokens` keep working without reaching for
/// `globalThis.tokens`.
///
/// The synthesised program shape:
/// ```ignore
/// (function () {
///   const __src = (<source>);
///   const __ctx = { cwd, env, currentToken, previousToken, tokens, searchTerm };
///   return __src(__ctx.tokens, __ctx);
/// })()
/// ```
fn build_script_function_program(source: &str) -> String {
    format!(
        "(function() {{ \
           const __src = ({src}); \
           const __cwd = typeof currentWorkingDirectory !== 'undefined' ? currentWorkingDirectory : ''; \
           const __env = typeof environmentVariables !== 'undefined' ? environmentVariables : {{}}; \
           const __ctx = {{ \
             cwd: __cwd, \
             currentWorkingDirectory: __cwd, \
             env: __env, \
             environmentVariables: __env, \
             currentToken: typeof currentToken !== 'undefined' ? currentToken : '', \
             previousToken: typeof previousToken !== 'undefined' ? previousToken : '', \
             tokens: typeof tokens !== 'undefined' ? tokens : [], \
             searchTerm: typeof searchTerm !== 'undefined' ? searchTerm : '', \
           }}; \
           return __src(__ctx.tokens, __ctx); \
         }})()",
        src = source,
    )
}

/// Construct the wrapper expression for a `custom` generator.
///
/// The JS source is an async function that takes
/// `(tokens, executeShellCommand, ctx)` and returns suggestions.
/// `executeShellCommand` and the ctx fields are also available as
/// top-level globals so legacy specs keep working.
///
/// The synthesised program returns the user function's eventual
/// promise. The worker awaits it via `Promise::finish` so async source
/// bodies resolve transparently.
fn build_custom_program(source: &str) -> String {
    format!(
        "(function() {{ \
           const __src = ({src}); \
           const __cwd = typeof currentWorkingDirectory !== 'undefined' ? currentWorkingDirectory : ''; \
           const __env = typeof environmentVariables !== 'undefined' ? environmentVariables : {{}}; \
           const __ctx = {{ \
             cwd: __cwd, \
             currentWorkingDirectory: __cwd, \
             env: __env, \
             environmentVariables: __env, \
             currentToken: typeof currentToken !== 'undefined' ? currentToken : '', \
             previousToken: typeof previousToken !== 'undefined' ? previousToken : '', \
             tokens: typeof tokens !== 'undefined' ? tokens : [], \
             searchTerm: typeof searchTerm !== 'undefined' ? searchTerm : '', \
           }}; \
           return __src(__ctx.tokens, executeShellCommand, __ctx); \
         }})()",
        src = source,
    )
}

/// Construct the wrapper expression for a `token_only` generator.
///
/// Converter-promoted sources are commonly original Fig function bodies
/// (`tokens => ...`), but hand-authored specs may also provide direct
/// expressions (`tokens.map(...)`). Support both forms without requiring the
/// converter to rewrite source text.
fn build_token_only_program(source: &str) -> String {
    format!(
        "(function() {{ \
           const __src = ({src}); \
           const __ctx = {{ \
             currentToken: typeof currentToken !== 'undefined' ? currentToken : '', \
             previousToken: typeof previousToken !== 'undefined' ? previousToken : '', \
             tokens: typeof tokens !== 'undefined' ? tokens : [], \
           }}; \
           if (typeof __src === 'function') {{ \
             if (__src.length >= 3) {{ \
               return __src(__ctx.tokens, undefined, __ctx); \
             }} \
             return __src(__ctx.tokens, __ctx); \
           }} \
           return __src; \
         }})()",
        src = source,
    )
}

/// Log a structured `tracing` event for each diagnostic so the doctor /
/// status surfaces can render them without re-implementing renderer logic.
/// Diagnostics never abort the suggestion pipeline; they are ALWAYS
/// warnings or info, never errors.
fn log_diagnostics(generator_id: &str, output: &JsRuntimeOutput) {
    for diag in &output.diagnostics {
        match diag.code {
            JsDiagnosticCode::Timeout => {
                tracing::warn!(
                    generator_id = %generator_id,
                    code = diag.code.tag(),
                    message = %diag.message,
                    "js_runtime: evaluation timed out"
                );
            }
            JsDiagnosticCode::MemoryExceeded => {
                tracing::warn!(
                    generator_id = %generator_id,
                    code = diag.code.tag(),
                    message = %diag.message,
                    "js_runtime: evaluation exceeded memory limit"
                );
            }
            JsDiagnosticCode::Exception => {
                tracing::warn!(
                    generator_id = %generator_id,
                    code = diag.code.tag(),
                    message = %diag.message,
                    "js_runtime: evaluation threw"
                );
            }
            JsDiagnosticCode::InvalidShape => {
                tracing::warn!(
                    generator_id = %generator_id,
                    code = diag.code.tag(),
                    message = %diag.message,
                    "js_runtime: unsupported return shape"
                );
            }
            JsDiagnosticCode::OversizedOutput => {
                tracing::warn!(
                    generator_id = %generator_id,
                    code = diag.code.tag(),
                    message = %diag.message,
                    "js_runtime: output cap exceeded"
                );
            }
            JsDiagnosticCode::UnsupportedApi => {
                tracing::warn!(
                    generator_id = %generator_id,
                    code = diag.code.tag(),
                    message = %diag.message,
                    "js_runtime: spec referenced a stripped API"
                );
            }
            JsDiagnosticCode::UnsupportedHostApi => {
                tracing::warn!(
                    generator_id = %generator_id,
                    code = diag.code.tag(),
                    api = %diag.message,
                    "js_runtime: spec called an unsupported host API"
                );
            }
            JsDiagnosticCode::ShellCommandStringDenied => {
                tracing::warn!(
                    generator_id = %generator_id,
                    code = diag.code.tag(),
                    message = %diag.message,
                    "js_runtime: shell-string command denied (allow_shell_command=false)"
                );
            }
            JsDiagnosticCode::ShellCommandLimitExceeded => {
                tracing::warn!(
                    generator_id = %generator_id,
                    code = diag.code.tag(),
                    message = %diag.message,
                    "js_runtime: executeShellCommand recursion cap reached"
                );
            }
            JsDiagnosticCode::ShellCommandFailed => {
                tracing::warn!(
                    generator_id = %generator_id,
                    code = diag.code.tag(),
                    message = %diag.message,
                    "js_runtime: executeShellCommand failed"
                );
            }
            JsDiagnosticCode::InvalidArgv => {
                tracing::warn!(
                    generator_id = %generator_id,
                    code = diag.code.tag(),
                    message = %diag.message,
                    "js_runtime: script_function returned invalid argv"
                );
            }
            // Empty output is a normal "no completions" path; demote to debug
            // so we don't spam logs during routine completions.
            JsDiagnosticCode::EmptyOutput => {
                tracing::debug!(
                    generator_id = %generator_id,
                    code = diag.code.tag(),
                    message = %diag.message,
                    "js_runtime: evaluation produced no suggestions"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_string_literal_round_trips_simple_string() {
        assert_eq!(json_string_literal("hello"), "\"hello\"");
    }

    #[test]
    fn json_string_literal_escapes_quotes_and_newlines() {
        let encoded = json_string_literal("a\"b\nc");
        // serde_json picks the canonical escapes — we just need the result to
        // be a valid JSON string token. Round-trip via a JSON parse to be
        // resilient to whichever escape style serde chose.
        let decoded: String = serde_json::from_str(&encoded).expect("valid json string");
        assert_eq!(decoded, "a\"b\nc");
    }

    #[test]
    fn build_post_process_program_invokes_source_with_string_literal() {
        let program = build_post_process_program("out => out.split('\\n')", "a\nb");
        // Sanity: the program is a single self-invocation passing the
        // encoded stdout literal to the body. The JSON encoder picks the
        // exact escape style; we just need the embedded string token to
        // start with `"` and the program to wrap a function call.
        assert!(program.starts_with("(("));
        assert!(program.ends_with("))"));
        assert!(
            program.contains("\"a"),
            "expected encoded stdout literal in program: {program}"
        );
    }

    #[test]
    fn build_token_only_program_supports_function_sources() {
        let program = build_token_only_program("tokens => tokens");
        assert!(program.contains("const __src = (tokens => tokens);"));
        assert!(program.contains("typeof __src === 'function'"));
        assert!(program.contains("if (__src.length >= 3)"));
        assert!(program.contains("return __src(__ctx.tokens, undefined, __ctx);"));
        assert!(program.contains("return __src(__ctx.tokens, __ctx);"));
    }

    #[test]
    fn build_token_only_program_supports_expression_sources() {
        let program = build_token_only_program("tokens.map(name => ({ name }))");
        assert!(program.contains("const __src = (tokens.map(name => ({ name })));"));
        assert!(program.contains("return __src;"));
    }

    /// Pins the perf contract for `worker()`: a burst of N concurrent
    /// callers must produce exactly one [`JsWorker::spawn`] invocation,
    /// even though every caller observes a `None` initial state. A
    /// regression that drops the `spawn_lock` (or replaces the
    /// double-check with a fence-less compare-exchange) would let
    /// multiple threads each spawn a 5 MB QuickJS runtime + OS thread
    /// before the OnceLock memoises the winner — invisible at the
    /// behavioural level, expensive in practice.
    #[test]
    fn worker_serialises_concurrent_spawn_attempts() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};
        use std::thread;

        let adapter = Arc::new(JsRuntimeAdapter::new());
        let spawn_count = Arc::new(AtomicUsize::new(0));
        // Synchronise threads so they all hit `worker_with` near-
        // simultaneously, maximising the window for a racing spawn.
        let barrier = Arc::new(Barrier::new(8));

        let mut handles = Vec::with_capacity(8);
        for _ in 0..8 {
            let adapter = Arc::clone(&adapter);
            let spawn_count = Arc::clone(&spawn_count);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                let _ = adapter.worker_with(|| {
                    spawn_count.fetch_add(1, Ordering::SeqCst);
                    JsWorker::spawn()
                });
            }));
        }
        for h in handles {
            h.join().expect("worker thread panicked");
        }

        assert_eq!(
            spawn_count.load(Ordering::SeqCst),
            1,
            "exactly one JsWorker spawn should occur under concurrent first-use"
        );
    }
}
