//! multipass native provider — replaces the JS-backed generator in
//! `specs/multipass.json` that shells out to `multipass list
//! --format=json` and projects the `name` field of each instance.
//!
//! `multipass list --format=json` emits a stable `{"list": [...]}`
//! envelope across versions (unlike arduino-cli, which has two wire
//! shapes). Each entry always carries `name`, `release`, `state`, and
//! `ipv4`; we model only the three we surface. `ipv4` and any future
//! top-level fields (`errors`, `status`, …) are ignored via serde's
//! default unknown-field behavior — keeps the struct small and
//! forward-compatible.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;
use tokio::process::Command;

use super::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

/// Timeout for `multipass list --format=json`. 2s matches the Phase 3A
/// default for external-tool subprocesses — comfortably above the local
/// daemon round-trip while tight enough that a stalled `multipassd`
/// can't block completion.
const MULTIPASS_LIST_TIMEOUT_MS: u64 = 2_000;

/// Top-level shape returned by `multipass list --format=json`.
#[derive(Debug, Deserialize)]
pub(crate) struct MultipassListOutput {
    list: Vec<MultipassInstance>,
}

/// Single instance row. Only the fields we surface are declared;
/// `ipv4` and any future top-level fields are ignored by serde's
/// default unknown-field behavior.
#[derive(Debug, Deserialize)]
struct MultipassInstance {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    release: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

/// Run `multipass list --format=json` against the user's real
/// `multipass` binary.
///
/// The `binary` argument is always `"multipass"` in production and
/// exists as a test seam: subprocess-failure tests inject a
/// nonexistent path so the spawn-time "file not found" path is
/// exercised without mutating `$PATH`.
pub(crate) async fn run_multipass_list_with_binary(
    cwd: &Path,
    binary: &str,
) -> Option<MultipassListOutput> {
    let output = match tokio::time::timeout(
        Duration::from_millis(MULTIPASS_LIST_TIMEOUT_MS),
        Command::new(binary)
            .args(["list", "--format=json"])
            .current_dir(cwd)
            .kill_on_drop(true)
            .output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::warn!("multipass list command failed: {e}");
            return None;
        }
        Err(_) => {
            tracing::warn!("multipass list timed out after {MULTIPASS_LIST_TIMEOUT_MS}ms");
            return None;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            exit = ?output.status.code(),
            stderr = %stderr.trim(),
            "multipass list failed"
        );
        return None;
    }

    match serde_json::from_slice::<MultipassListOutput>(&output.stdout) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("multipass list JSON parse failed: {e}");
            None
        }
    }
}

/// Pure parsing step — exposed only to tests so they can exercise the
/// malformed-JSON branch without spawning a subprocess.
#[cfg(test)]
fn parse_multipass_list_raw(json: &str) -> Option<MultipassListOutput> {
    serde_json::from_str(json).ok()
}

/// Project instance rows into suggestions. Pure so tests can cover
/// every degradation path (missing name, missing release, missing
/// state) without a subprocess.
fn instances_to_suggestions(output: MultipassListOutput) -> Vec<Suggestion> {
    output
        .list
        .into_iter()
        .filter_map(|inst| {
            let name = inst.name?;
            let release = inst.release.as_deref().unwrap_or("");
            let state = inst.state.as_deref().unwrap_or("");
            let description = match (release.is_empty(), state.is_empty()) {
                (true, true) => None,
                (true, false) => Some(format!("({state})")),
                (false, true) => Some(release.to_string()),
                (false, false) => Some(format!("{release} ({state})")),
            };
            Some(Suggestion {
                text: name,
                description,
                kind: SuggestionKind::ProviderValue,
                source: SuggestionSource::Provider,
                ..Default::default()
            })
        })
        .collect()
}

/// `multipass list --format=json` — enumerates multipass instances by
/// name. Replaces the `requires_js: true` generator in
/// `specs/multipass.json` whose JS source projected `name` out of each
/// entry in `output.list`.
pub struct MultipassList;

impl Provider for MultipassList {
    fn name(&self) -> &'static str {
        "multipass_list"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "multipass").await
    }
}

impl MultipassList {
    /// Test seam — `generate` calls this with `"multipass"`; tests
    /// call it with a nonexistent path to exercise the
    /// spawn-failure → `Ok(vec![])` fallback contract without mutating
    /// `$PATH`.
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        let Some(output) = run_multipass_list_with_binary(&ctx.cwd, binary).await else {
            return Ok(Vec::new());
        };
        Ok(instances_to_suggestions(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_happy_path() {
        let json = r#"{
            "list": [
                {"name": "primary", "release": "22.04 LTS", "state": "Running", "ipv4": ["10.0.0.1"]},
                {"name": "dev-box", "release": "20.04 LTS", "state": "Stopped", "ipv4": []}
            ]
        }"#;
        let output = parse_multipass_list_raw(json).expect("parse should succeed");
        let suggestions = instances_to_suggestions(output);
        assert_eq!(suggestions.len(), 2);
        assert_eq!(suggestions[0].text, "primary");
        assert_eq!(
            suggestions[0].description.as_deref(),
            Some("22.04 LTS (Running)")
        );
        assert_eq!(suggestions[0].kind, SuggestionKind::ProviderValue);
        assert_eq!(suggestions[0].source, SuggestionSource::Provider);
        assert_eq!(suggestions[1].text, "dev-box");
        assert_eq!(
            suggestions[1].description.as_deref(),
            Some("20.04 LTS (Stopped)")
        );
    }

    #[test]
    fn parse_empty_list() {
        let output = parse_multipass_list_raw(r#"{"list": []}"#).expect("parse should succeed");
        assert!(instances_to_suggestions(output).is_empty());
    }

    #[test]
    fn parse_missing_name_filtered() {
        // Defensive: multipass shouldn't emit a null `name`, but if it
        // ever did (or if a future field rename broke deserialization
        // into Option::None), the entry must be dropped rather than
        // producing a nameless suggestion.
        let json = r#"{
            "list": [
                {"name": null, "release": "22.04 LTS", "state": "Running"},
                {"name": "keeper", "release": "22.04 LTS", "state": "Running"}
            ]
        }"#;
        let output = parse_multipass_list_raw(json).expect("parse should succeed");
        let suggestions = instances_to_suggestions(output);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].text, "keeper");
    }

    #[test]
    fn parse_missing_release_or_state_graceful() {
        // Three degradation paths: missing release, missing state, and
        // both missing. Description must degrade to something
        // meaningful (or None) — never emit `undefined` or similar
        // sentinel strings.
        let json = r#"{
            "list": [
                {"name": "no-release", "state": "Running"},
                {"name": "no-state", "release": "22.04 LTS"},
                {"name": "neither"}
            ]
        }"#;
        let output = parse_multipass_list_raw(json).expect("parse should succeed");
        let suggestions = instances_to_suggestions(output);
        assert_eq!(suggestions.len(), 3);
        assert_eq!(suggestions[0].text, "no-release");
        assert_eq!(suggestions[0].description.as_deref(), Some("(Running)"));
        assert_eq!(suggestions[1].text, "no-state");
        assert_eq!(suggestions[1].description.as_deref(), Some("22.04 LTS"));
        assert_eq!(suggestions[2].text, "neither");
        assert_eq!(suggestions[2].description, None);
        for s in &suggestions {
            if let Some(d) = s.description.as_deref() {
                assert!(
                    !d.contains("undefined") && !d.contains("null"),
                    "description leaked sentinel: {d:?}"
                );
            }
        }
    }

    #[test]
    fn parse_malformed_json_returns_none() {
        assert!(parse_multipass_list_raw("not json").is_none());
        assert!(parse_multipass_list_raw("").is_none());
        assert!(parse_multipass_list_raw("{").is_none());
    }

    #[tokio::test]
    async fn subprocess_failure_returns_none() {
        // Exercises the spawn-time "file not found" path by injecting
        // a binary name that cannot resolve anywhere on disk. No
        // global state mutated — safe to run in parallel.
        let tmp = tempfile::TempDir::new().unwrap();
        let result = run_multipass_list_with_binary(
            tmp.path(),
            "/nonexistent/multipass-definitely-not-real",
        )
        .await;
        assert!(
            result.is_none(),
            "expected None when the multipass binary does not exist"
        );
    }

    #[tokio::test]
    async fn generate_returns_ok_empty_when_binary_missing() {
        // End-to-end: `MultipassList::generate` MUST translate a
        // spawn-time failure into `Ok(vec![])`. A silent shift to
        // `Err` would stall the completion pipeline.
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ProviderCtx {
            cwd: tmp.path().to_path_buf(),
            env: std::sync::Arc::new(std::collections::HashMap::new()),
            current_token: String::new(),
        };
        let result = MultipassList
            .generate_with_binary(&ctx, "/nonexistent/multipass-for-test")
            .await;
        assert!(matches!(result, Ok(ref v) if v.is_empty()));
    }
}
