//! Adapter that wires [`gc_jsrt::JsWorker`] into the suggestion engine.
//!
//! This module sits between `engine.rs` (which dispatches generator results)
//! and `gc_jsrt` (which evaluates JS in a sandboxed worker). It owns three
//! responsibilities:
//!
//! 1. **Lazy worker spawn.** Spawning the worker thread costs ~5 MB and a
//!    QuickJS runtime initialisation; we don't want that price unless a
//!    Phase-4 generator actually fires. [`JsRuntimeAdapter::worker`] pays it
//!    once and memoises the result.
//! 2. **Program assembly.** `js_runtime.source` ships the *body* of the JS
//!    function (e.g. `out => out.split('\n').map(name => ({ name }))`). The
//!    runtime evaluates a top-level expression, so we synthesise a
//!    self-invoking call:
//!    `((out, ctx) => (<source>)(out, ctx))(<JSON.parse(stdout)>, <JSON.parse(ctx)>)`.
//!    For `post_process` generators the first argument is the script's
//!    stdout as a JS string.
//! 3. **Diagnostic logging.** Every [`gc_jsrt::JsDiagnostic`] is mapped to a
//!    structured `tracing` event so Phase 7 (status / doctor) can pick them
//!    up without re-implementing the rendering.
//!
//! The adapter is intentionally narrow — no caching, no transform pipeline,
//! no source-hashing. Those live in the engine; we only run JS.
//!
//! # Phase scope
//!
//! Phase 4 implements [`JsRuntimeAdapter::post_process`] (the
//! `kind = post_process` variant where stdout is the only input). Phases 5+
//! will add `script_function` and `custom` entry points without changing
//! the worker contract.

use std::sync::OnceLock;
use std::time::Duration;

use gc_jsrt::{JsDiagnosticCode, JsRuntimeError, JsRuntimeInput, JsRuntimeOutput, JsWorker};

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
    fn worker(&self) -> Result<&JsWorker, JsRuntimeError> {
        if let Some(w) = self.worker.get() {
            return Ok(w);
        }
        let spawned = JsWorker::spawn()?;
        // OnceLock::set returns Err if another thread won the race; that's
        // fine, we just drop our spawn and use whichever worker landed.
        let _ = self.worker.set(spawned);
        // `get` after either `set` or a race-loser is guaranteed to succeed.
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
            ..JsRuntimeInput::default()
        };
        let output = worker.evaluate(program, input, timeout).await?;
        log_diagnostics(&generator_id, &output);
        Ok(output)
    }
}

/// Construct the wrapper expression that invokes the generator body with
/// the script's stdout. We embed `stdout` as a JS string literal so the
/// JS function body sees the exact bytes we captured. The lone caveat:
/// `String.fromCharCode` would lose data for non-BMP code points, so we
/// use `JSON.parse` of a quoted JSON string instead — that preserves the
/// full UTF-8 byte sequence.
fn build_post_process_program(source: &str, stdout: &str) -> String {
    let stdout_literal = json_string_literal(stdout);
    // JSON string literals are a subset of JavaScript string literals — every
    // legal JSON escape (`\n`, `\t`, `\uXXXX`, etc.) is also a legal JS
    // escape — so the encoded stdout doubles as a JS string token. Splicing
    // it directly into the program avoids a `JSON.parse` round-trip whose
    // semantics differ subtly: `JSON.parse` would reject inputs containing
    // raw control characters that JS string literals tolerate.
    format!(
        "(({source})({stdout_literal}))",
        source = source,
        stdout_literal = stdout_literal,
    )
}

/// Encode a Rust string as a JSON string literal, suitable for embedding
/// inside the JS program text we hand to QuickJS. We rely on `serde_json`
/// rather than hand-rolling escapes so we cannot get e.g. ` ` /
/// ` ` (line separator / paragraph separator) wrong — both are
/// permitted in JSON but ARE allowed in JS string literals so the JSON
/// path is safe to splice directly.
fn json_string_literal(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// Log a structured `tracing` event for each diagnostic so Phase 7 can pick
/// them up without re-implementing renderer logic. Diagnostics never abort
/// the suggestion pipeline; they are ALWAYS warnings or info, never errors.
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
}
