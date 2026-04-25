//! Thin wrapper around [`crate::transform::execute_pipeline`] with the
//! argument order and return type preferred by the fig-converter oracle
//! (`tools/fig-converter/src/oracle.js`).
//!
//! The oracle compares Rust transform-pipeline output against a `node:vm`
//! execution of the original JS generator source. It spawns
//! `cargo run --example run-transforms` and passes `{ transforms, input }`
//! JSON on stdin — that shape maps naturally to
//! `try_run_pipeline(transforms, input)`.
//!
//! # Error semantics
//!
//! [`try_run_pipeline`] preserves the underlying [`execute_pipeline`] error
//! as a `String` so the oracle binary can serialize a `{ "error": ... }`
//! payload and distinguish "pipeline errored" from "pipeline legitimately
//! produced nothing". Today [`execute_pipeline`] returns `Ok` on every
//! branch, but the signature keeps the error channel open for future
//! transforms that may fail.

use crate::transform::{execute_pipeline, Transform};
use crate::types::Suggestion;

/// Run a transform pipeline against raw input, preserving pipeline errors.
///
/// Used by the `run-transforms` example binary so it can serialize a
/// `{ "error": ... }` payload for the oracle.
pub fn try_run_pipeline(transforms: &[Transform], input: &str) -> Result<Vec<Suggestion>, String> {
    execute_pipeline(input, transforms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform::{NamedTransform, ParameterizedTransform};

    #[test]
    fn try_run_pipeline_happy_path_split_and_filter() {
        let transforms = vec![
            Transform::Named(NamedTransform::SplitLines),
            Transform::Named(NamedTransform::FilterEmpty),
        ];
        let out = try_run_pipeline(&transforms, "a\nb\n\nc\n").unwrap();
        let names: Vec<&str> = out.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn try_run_pipeline_empty_input_returns_empty() {
        let transforms = vec![
            Transform::Named(NamedTransform::SplitLines),
            Transform::Named(NamedTransform::FilterEmpty),
        ];
        let out = try_run_pipeline(&transforms, "").unwrap();
        assert!(out.is_empty(), "expected empty Vec, got {out:?}");
    }

    #[test]
    fn try_run_pipeline_error_guard_short_circuits_to_empty() {
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
        let out = try_run_pipeline(&transforms, "Error: boom\n").unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn try_run_pipeline_surfaces_result() {
        // The Result-preserving signature must hand back the full vec on
        // the happy path, and callers can distinguish `Ok(empty)` from
        // an eventual future `Err`.
        let transforms = vec![
            Transform::Named(NamedTransform::SplitLines),
            Transform::Named(NamedTransform::FilterEmpty),
        ];
        let res = try_run_pipeline(&transforms, "x\ny\n").unwrap();
        let names: Vec<&str> = res.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(names, vec!["x", "y"]);
    }
}
