//! Thin wrapper around [`crate::transform::execute_pipeline`] with the
//! argument order and return type preferred by the Phase 0 Node-side oracle
//! (`tools/requires-js-oracle`).
//!
//! The oracle compares Rust transform-pipeline output against a `node:vm`
//! execution of the original JS generator source. It spawns
//! `cargo run --example run-transforms` and passes `{ transforms, input }`
//! JSON on stdin — that shape maps naturally to `run_pipeline(transforms, input)`.
//!
//! # Error semantics
//!
//! The public signature returns `Vec<Suggestion>` directly (no `Result`) so the
//! oracle can always round-trip a JSON array. When the underlying
//! [`execute_pipeline`] fails (today this is unreachable in practice — it
//! returns `Ok` on every branch — but the signature allows errors), the error
//! message is written to stderr via `eprintln!` and an empty `Vec` is returned.
//!
//! That mapping is deliberate: the Node oracle classifies a generator as
//! `fail` whenever the Rust and JS outputs disagree; returning `Vec::new()`
//! surfaces a bug as a diff mismatch rather than silently succeeding, while
//! stderr preserves the Rust error detail for humans to read.
//!
//! Callers that need the raw error (e.g. the example binary, which wants to
//! distinguish "pipeline errored" from "pipeline legitimately produced
//! nothing") should use [`try_run_pipeline`] instead.

use crate::transform::{execute_pipeline, Transform};
use crate::types::Suggestion;

/// Run a transform pipeline against raw input, discarding errors.
///
/// Errors from [`execute_pipeline`] are logged to stderr and turned into an
/// empty `Vec<Suggestion>`. See the module docs for the rationale.
pub fn run_pipeline(transforms: &[Transform], input: &str) -> Vec<Suggestion> {
    match execute_pipeline(input, transforms) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("run_pipeline: {e}");
            Vec::new()
        }
    }
}

/// Run a transform pipeline against raw input, preserving pipeline errors.
///
/// Used by the `run-transforms` example binary so it can serialize a
/// `{ "error": ... }` payload for the oracle. Prefer [`run_pipeline`] when you
/// don't need the error detail.
pub fn try_run_pipeline(transforms: &[Transform], input: &str) -> Result<Vec<Suggestion>, String> {
    execute_pipeline(input, transforms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform::{NamedTransform, ParameterizedTransform};

    #[test]
    fn run_pipeline_happy_path_split_and_filter() {
        let transforms = vec![
            Transform::Named(NamedTransform::SplitLines),
            Transform::Named(NamedTransform::FilterEmpty),
        ];
        let out = run_pipeline(&transforms, "a\nb\n\nc\n");
        let names: Vec<&str> = out.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn run_pipeline_empty_input_returns_empty_or_single_blank() {
        // With no transforms, execute_pipeline wraps the raw input as a single
        // suggestion. With split_lines+filter_empty on an empty string we get
        // an empty Vec — that's the interesting "nothing to do" case.
        let transforms = vec![
            Transform::Named(NamedTransform::SplitLines),
            Transform::Named(NamedTransform::FilterEmpty),
        ];
        let out = run_pipeline(&transforms, "");
        assert!(out.is_empty(), "expected empty Vec, got {out:?}");
    }

    #[test]
    fn run_pipeline_error_guard_short_circuits_to_empty() {
        // error_guard aborts the pipeline and returns an empty Vec — this is
        // `Ok(Vec::new())` at the executor layer, not an `Err`, but it's the
        // closest user-visible "no output" case and worth pinning.
        let transforms = vec![
            Transform::Parameterized(ParameterizedTransform::ErrorGuard {
                starts_with: Some("Error:".into()),
                contains: None,
            }),
            Transform::Named(NamedTransform::SplitLines),
        ];
        let out = run_pipeline(&transforms, "Error: boom\n");
        assert!(out.is_empty());
    }

    #[test]
    fn try_run_pipeline_surfaces_result() {
        // The Result-preserving variant must produce identical output to
        // run_pipeline on the happy path, and callers can distinguish Ok(empty)
        // from an eventual future Err.
        let transforms = vec![
            Transform::Named(NamedTransform::SplitLines),
            Transform::Named(NamedTransform::FilterEmpty),
        ];
        let res = try_run_pipeline(&transforms, "x\ny\n").unwrap();
        let names: Vec<&str> = res.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(names, vec!["x", "y"]);
    }
}
