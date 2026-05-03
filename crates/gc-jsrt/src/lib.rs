//! Bounded QuickJS evaluator for Ghost Complete.
//!
//! This crate ships the [`JsWorker`] sandbox used by the UX-9
//! `requires_js` dispatch path. Phase 3 landed the foundation, Phase 4
//! wired the `PostProcess` (stdout-in / suggestions-out) shape, and
//! Phase 5 wires the remaining `ScriptFunction` and `Custom` shapes
//! plus the synchronous host API (cwd / env / tokens /
//! `executeShellCommand`).
//!
//! # Design highlights
//!
//! - A dedicated worker thread owns one [`rquickjs::Runtime`] for the
//!   lifetime of the worker. The Tokio side talks to it through an
//!   [`std::sync::mpsc`] queue plus per-job [`tokio::sync::oneshot`]
//!   reply channels. JS evaluation is fully synchronous — we never
//!   need `AsyncRuntime`, because the JS we run is short, pure
//!   post-processing or argv-producing logic.
//! - Each job opens a **fresh `Context`** so two unrelated specs cannot
//!   pollute each other's globals. Reusing the runtime keeps GC warm
//!   and avoids allocator churn between jobs.
//! - The sandbox strips Node-style globals (`require`, `process`,
//!   `Deno`, `Bun`, `setTimeout`, `fetch`, etc.) and replaces `eval` and
//!   `Function` with throwers — defense in depth even though spec JS is
//!   not expected to use them.
//! - A wall-clock budget is enforced via
//!   [`rquickjs::Runtime::set_interrupt_handler`]. Note this is
//!   **bounded but not hard real-time**: long native operations
//!   (pathological regex, JSON parsing of huge strings) may not be
//!   preempted at the exact deadline. Combine with output caps and
//!   memory limits.
//! - Output normalization rejects anything we cannot turn into
//!   `name`+`description?` suggestions, including cyclic objects (JSON
//!   serialization throws), oversized payloads, and host objects.
//!
//! # Public surface
//!
//! [`JsWorker`] is the entry point. Construct with [`JsWorker::spawn`],
//! evaluate with [`JsWorker::evaluate`].

mod host;
mod normalize;
mod sandbox;
mod types;
mod worker;

pub use normalize::{MAX_DESCRIPTION_LEN, MAX_NAME_LEN, MAX_SUGGESTIONS, MAX_TOTAL_OUTPUT_BYTES};
pub use types::{
    JsDiagnostic, JsDiagnosticCode, JsExecutionKind, JsRuntimeError, JsRuntimeInput,
    JsRuntimeOutput, JsSuggestion, ShellRunError, ShellRunOutput, ShellRunner,
};
pub use worker::{JsWorker, MAX_SHELL_CALLS_PER_EVALUATION};
