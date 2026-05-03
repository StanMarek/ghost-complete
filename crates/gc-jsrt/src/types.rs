//! Public types for [`crate::JsWorker`].
//!
//! Kept in their own module so downstream callers in `gc-suggest` (Phase 4+)
//! can `use gc_jsrt::{JsRuntimeInput, JsRuntimeOutput, ...}` without pulling
//! in the worker implementation details.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Input handed to a JS evaluation job.
///
/// All fields are populated by the caller (Phase 4+ in `gc-suggest`):
/// Phase 3 only validates the type carries forward correctly.
#[derive(Debug, Clone, Default)]
pub struct JsRuntimeInput {
    /// Captured stdout from a script generator. Populated for the
    /// `post_process` flavour where stdout feeds the JS function.
    pub stdout: Option<String>,
    /// Tokens from the parsed command line. Populated for
    /// `script_function` / `custom` flavours.
    pub tokens: Vec<String>,
    /// The current word the user is typing.
    pub current_token: String,
    /// Working directory for the shell.
    pub cwd: PathBuf,
    /// Filtered environment (whitelist applied by the caller; Phase 5
    /// will define the canonical filter set).
    pub env: BTreeMap<String, String>,
    /// Human-readable identifier for diagnostics. Typically
    /// `<spec-id>:<generator-index>`.
    pub generator_id: String,
}

/// Result of a JS evaluation job.
#[derive(Debug, Clone, Default)]
pub struct JsRuntimeOutput {
    /// Normalized suggestions, ready for fuzzy ranking.
    pub suggestions: Vec<JsSuggestion>,
    /// Non-fatal observations (truncation, exception text, etc.).
    pub diagnostics: Vec<JsDiagnostic>,
}

impl JsRuntimeOutput {
    pub(crate) fn empty_with(diagnostic: JsDiagnostic) -> Self {
        Self {
            suggestions: Vec::new(),
            diagnostics: vec![diagnostic],
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
