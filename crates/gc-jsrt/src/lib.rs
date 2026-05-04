//! Bounded QuickJS evaluator for Ghost Complete's `requires_js` specs.
//!
//! No module loader, no Node globals, no host I/O beyond the synchronous
//! `executeShellCommand` binding.

mod host;
mod normalize;
mod sandbox;
mod types;
mod worker;

pub use normalize::{MAX_DESCRIPTION_LEN, MAX_NAME_LEN, MAX_SUGGESTIONS, MAX_TOTAL_OUTPUT_BYTES};
pub use types::{
    JsDiagnostic, JsDiagnosticCode, JsExecutionKind, JsRuntimeError, JsRuntimeInput,
    JsRuntimeOutput, JsRuntimeOutputPayload, JsSuggestion, ShellRunError, ShellRunOutput,
    ShellRunner,
};
pub use worker::{JsWorker, MAX_SHELL_CALLS_PER_EVALUATION};
