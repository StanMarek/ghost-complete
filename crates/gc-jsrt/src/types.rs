//! Public types for [`crate::JsWorker`].
//!
//! Kept in their own module so downstream callers in `gc-suggest` (Phase 4+)
//! can `use gc_jsrt::{JsRuntimeInput, JsRuntimeOutput, ...}` without pulling
//! in the worker implementation details.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Outcome of a single shell command initiated from JS through the host
/// `executeShellCommand` binding.
///
/// Mirrors the surface a Fig spec author would see if they were running
/// in the original Fig host: a string of stdout, the exit status, and
/// stderr surfaced separately for completeness. Soft-failure semantics —
/// non-zero exit produces an `Err`, never a panic, so Phase 5's bounded
/// recursion cap can deny further calls cleanly.
#[derive(Debug, Clone, Default)]
pub struct ShellRunOutput {
    /// Captured stdout (already decoded as UTF-8 lossy by the caller).
    pub stdout: String,
    /// Captured stderr — present even on success for diagnostic
    /// surfacing in JS via the `stderr` field.
    pub stderr: String,
    /// Exit status: `Some(n)` when the child terminated with a code,
    /// `None` when killed by signal.
    pub exit_code: Option<i32>,
}

/// Failure modes the host `ShellRunner` can return synchronously into
/// the JS worker.
///
/// Each variant maps to a JS-side exception with a stable diagnostic
/// code so the Phase 7 doctor / status views can bucket runtime
/// failures without re-parsing message strings.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ShellRunError {
    /// Caller passed a string command and the spec did not opt into
    /// `allow_shell_command`. Surfaced to JS as `ShellCommandStringDenied`.
    #[error("shell-string command denied (allow_shell_command=false)")]
    StringDenied,
    /// Argv parse failure (e.g. unmatched quote in shell-string mode).
    #[error("could not parse shell command: {0}")]
    ArgvParse(String),
    /// Spawn-time IO error.
    #[error("spawn failed: {0}")]
    Spawn(String),
    /// Wall-clock timeout exhausted before child exit.
    #[error("shell command timed out")]
    Timeout,
    /// Child exited with non-zero status. The caller surfaces stdout +
    /// stderr alongside the exit code so JS can decide what to do.
    #[error("shell command exited with status {exit_code:?}")]
    NonZeroExit {
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    /// Catch-all for unexpected runner failures (worker dropped the
    /// runner, etc.).
    #[error("internal shell runner error: {0}")]
    Internal(String),
}

/// Synchronous host hook the JS worker calls when a `script_function`
/// returns argv or a `custom` generator invokes `executeShellCommand`.
///
/// Implementations live in `gc-suggest`; the worker only sees the trait
/// object so the JS crate stays free of `tokio::process` / `script::run_script`
/// concerns. The contract is intentionally synchronous: the worker thread
/// is a dedicated OS thread (not a tokio task), so blocking it on a
/// `Handle::block_on(run_script(...))` is safe — the JS evaluation
/// budget is bounded by the same wall-clock timeout that gates the
/// outer JS interpreter.
///
/// The host layer enforces `js_runtime.allow_shell_command` before it
/// calls [`Self::run_string`]. [`Self::run_argv`] is always argv-only,
/// and the default [`Self::run_string`] implementation denies
/// shell-string commands unless a runner explicitly opts in.
pub trait ShellRunner: Send + Sync {
    /// Execute a shell command in argv form. Implementations MUST exec
    /// via an argv array (no `sh -c`), use the supplied `cwd`, and
    /// honour `timeout`.
    fn run_argv(
        &self,
        argv: &[String],
        cwd: &std::path::Path,
        timeout: Duration,
    ) -> Result<ShellRunOutput, ShellRunError>;

    /// Execute a shell command supplied as a single string (e.g.
    /// `"ls -la /tmp"`). The runner is responsible for parsing into argv
    /// (typically via `shell-words::split`) and then exec'ing.
    /// Implementations SHOULD reject inputs that contain shell control
    /// characters that require true shell semantics.
    ///
    /// The default implementation returns [`ShellRunError::StringDenied`]
    /// — opting in requires an override on a runner that has audited
    /// the input source.
    fn run_string(
        &self,
        _command: &str,
        _cwd: &std::path::Path,
        _timeout: Duration,
    ) -> Result<ShellRunOutput, ShellRunError> {
        Err(ShellRunError::StringDenied)
    }
}

/// Selects the JS dispatch shape the worker should use for a given job.
///
/// Phase 4 covered only [`Self::PostProcess`]. Phase 5 introduces the
/// remaining two shapes and reuses the shared sandbox / interrupt
/// machinery for all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JsExecutionKind {
    /// JS receives the script's stdout and returns suggestions.
    /// (Phase 4.)
    #[default]
    PostProcess,
    /// JS receives the parsed tokens + context and returns argv. The
    /// engine then runs the argv as a normal script generator and feeds
    /// the stdout through the optional transform pipeline. (Phase 5.)
    ScriptFunction,
    /// JS receives the parsed tokens + a host `executeShellCommand`
    /// binding and returns suggestions directly. May spawn 0–N child
    /// processes during evaluation. (Phase 5.)
    Custom,
}

/// Input handed to a JS evaluation job.
///
/// All fields are populated by the caller (Phase 4+ in `gc-suggest`).
/// Phase 5 wired the `tokens` / `current_token` / `cwd` / `env` /
/// `previous_token` fields to the host bindings; Phase 4 only reads
/// `stdout`.
#[derive(Clone, Default)]
pub struct JsRuntimeInput {
    /// Captured stdout from a script generator. Populated for the
    /// `post_process` flavour where stdout feeds the JS function.
    pub stdout: Option<String>,
    /// Tokens from the parsed command line. Populated for
    /// `script_function` / `custom` flavours.
    pub tokens: Vec<String>,
    /// The current word the user is typing.
    pub current_token: String,
    /// The token immediately before `current_token` (the typical Fig
    /// `previousToken` field). Empty when the user has typed nothing
    /// after the command name.
    pub previous_token: String,
    /// Working directory for the shell.
    pub cwd: PathBuf,
    /// Filtered environment (whitelist applied by the caller; the
    /// Phase 5 dispatch path strips `GHOST_COMPLETE_ACTIVE` and surfaces
    /// the rest verbatim).
    pub env: BTreeMap<String, String>,
    /// Human-readable identifier for diagnostics. Typically
    /// `<spec-id>:<generator-index>`.
    pub generator_id: String,
    /// Shape selector. `PostProcess` (Phase 4) is the historical default;
    /// `ScriptFunction` / `Custom` are new in Phase 5 and require the
    /// optional fields above.
    pub kind: JsExecutionKind,
    /// Whether a `Custom` generator may pass a shell-string to
    /// `executeShellCommand`. Mirrors the spec's
    /// `js_runtime.allow_shell_command` flag.
    pub allow_shell_command: bool,
    /// Synchronous host hook used by `ScriptFunction` (post-argv
    /// resolution skipped — handled in the engine) and `Custom`
    /// (per-call). When `None`, any in-JS call to `executeShellCommand`
    /// raises a `ShellCommandFailed` diagnostic — the variant is held
    /// open so unit tests can run a worker without a runner. The
    /// production engine always supplies one.
    pub shell_runner: Option<Arc<dyn ShellRunner>>,
}

impl std::fmt::Debug for JsRuntimeInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsRuntimeInput")
            .field("stdout_len", &self.stdout.as_ref().map(|s| s.len()))
            .field("tokens", &self.tokens)
            .field("current_token", &self.current_token)
            .field("previous_token", &self.previous_token)
            .field("cwd", &self.cwd)
            .field("env_len", &self.env.len())
            .field("generator_id", &self.generator_id)
            .field("kind", &self.kind)
            .field("allow_shell_command", &self.allow_shell_command)
            .field("shell_runner_installed", &self.shell_runner.is_some())
            .finish()
    }
}

/// Result of a JS evaluation job.
#[derive(Debug, Clone, Default)]
pub struct JsRuntimeOutput {
    /// Normalized suggestions, ready for fuzzy ranking.
    pub suggestions: Vec<JsSuggestion>,
    /// Non-fatal observations (truncation, exception text, etc.).
    pub diagnostics: Vec<JsDiagnostic>,
    /// Resolved argv when the worker ran a `ScriptFunction` job. Empty
    /// for `PostProcess` and `Custom` jobs. The engine reads this to
    /// decide whether to spawn a follow-up script generator.
    pub argv: Vec<String>,
}

impl JsRuntimeOutput {
    pub(crate) fn empty_with(diagnostic: JsDiagnostic) -> Self {
        Self {
            suggestions: Vec::new(),
            diagnostics: vec![diagnostic],
            argv: Vec::new(),
        }
    }
}

/// One normalized suggestion produced by a JS generator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsSuggestion {
    /// Required. Surfaced to nucleo for ranking.
    pub name: String,
    /// Optional secondary text shown in the popup.
    pub description: Option<String>,
}

/// Non-fatal observation surfaced alongside (or instead of) suggestions.
///
/// Diagnostics never abort the suggestion engine — a generator that
/// times out simply returns no suggestions plus a [`JsDiagnosticCode::Timeout`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsDiagnostic {
    pub code: JsDiagnosticCode,
    pub message: String,
}

/// Categorised reason for a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsDiagnosticCode {
    /// The interrupt handler aborted execution because the wall-clock
    /// deadline elapsed.
    Timeout,
    /// QuickJS reported memory exhaustion.
    MemoryExceeded,
    /// The JS return value did not match any supported shape.
    InvalidShape,
    /// The serialized output exceeded [`crate::MAX_TOTAL_OUTPUT_BYTES`]
    /// or the array exceeded [`crate::MAX_SUGGESTIONS`].
    OversizedOutput,
    /// JS code threw an uncaught exception.
    Exception,
    /// JS code attempted to use a stripped/disabled global (e.g. `fetch`).
    UnsupportedApi,
    /// JS evaluated to `undefined` / `null` / an empty array. Distinct
    /// from `InvalidShape` because the Phase 4+ dispatch path may treat
    /// it as an empty success.
    EmptyOutput,
    /// Phase 5: JS reached for a host API the runtime does not expose
    /// (e.g. `fig.fs.readFile`). The string slot identifies the API name
    /// for telemetry without leaking arguments.
    UnsupportedHostApi,
    /// Phase 5: a `Custom` generator called `executeShellCommand("…")`
    /// with the shell-string form when `allow_shell_command` was false.
    ShellCommandStringDenied,
    /// Phase 5: a `Custom` generator exceeded the per-evaluation
    /// `executeShellCommand` recursion cap.
    ShellCommandLimitExceeded,
    /// Phase 5: `executeShellCommand` returned a non-zero exit, timed
    /// out, or otherwise failed to produce stdout. The JS-level call
    /// throws so the spec author can catch it; this diagnostic is the
    /// fallback when it bubbles up uncaught.
    ShellCommandFailed,
    /// Phase 5: a `script_function` generator returned something other
    /// than a non-empty argv array. Distinct from `InvalidShape` so the
    /// engine can keep its argv-validation rules close to home.
    InvalidArgv,
}

impl JsDiagnosticCode {
    /// Short tag for telemetry / logs.
    pub fn tag(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::MemoryExceeded => "memory_exceeded",
            Self::InvalidShape => "invalid_shape",
            Self::OversizedOutput => "oversized_output",
            Self::Exception => "exception",
            Self::UnsupportedApi => "unsupported_api",
            Self::EmptyOutput => "empty_output",
            Self::UnsupportedHostApi => "unsupported_host_api",
            Self::ShellCommandStringDenied => "shell_command_string_denied",
            Self::ShellCommandLimitExceeded => "shell_command_limit_exceeded",
            Self::ShellCommandFailed => "shell_command_failed",
            Self::InvalidArgv => "invalid_argv",
        }
    }
}

/// Fatal error from the worker.
///
/// Soft conditions (timeout, oversized output, bad shape) are reported
/// as [`JsDiagnostic`]s on a successful [`JsRuntimeOutput`]; this enum
/// is reserved for situations where we cannot return any output at all.
#[derive(Debug, thiserror::Error)]
pub enum JsRuntimeError {
    /// The worker thread is gone — either it panicked, the channel was
    /// closed, or `spawn` failed.
    #[error("gc-jsrt worker thread is not running")]
    WorkerDead,
    /// We could not even start evaluation (e.g. context creation failed).
    #[error("internal gc-jsrt error: {0}")]
    Internal(String),
}
