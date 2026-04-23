//! ansible-doc native provider — replaces the JS-backed generator in
//! `specs/ansible-doc.json` that shells out to
//! `ansible-doc --list --json` and projects the map of fully-qualified
//! module names to their one-line descriptions.
//!
//! Wire shape: a flat JSON object whose keys are module names (e.g.
//! `ansible.builtin.apt`) and whose values are short description
//! strings. This is the format produced by `ansible-doc --list --json`
//! in ansible-core >= 2.10. Older versions emit a namespace-grouped
//! envelope; we deliberately do not attempt to handle those — if parse
//! fails the provider returns empty, matching the pattern used by every
//! other Phase 3A provider when the external tool misbehaves.
//!
//! The payload maps cleanly onto `BTreeMap<String, String>` via serde's
//! built-in map support, so no explicit `#[derive(Deserialize)]` type
//! is needed. `BTreeMap` (rather than `HashMap`) is chosen for
//! deterministic alphabetical ordering — the output is small enough
//! (~1–3k entries) that the ordering cost is immaterial, and it keeps
//! test assertions on `suggestions[0].text` stable.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use tokio::process::Command;

use super::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

/// Timeout for `ansible-doc --list --json`. 2s matches the Phase 3A
/// default for external-tool subprocesses — comfortably above the
/// module-index scan time on a typical ansible install while tight
/// enough that a misconfigured collection path cannot block completion.
const ANSIBLE_DOC_LIST_TIMEOUT_MS: u64 = 2_000;

/// The wire shape of `ansible-doc --list --json`: a flat map of
/// fully-qualified module name → one-line description.
pub(crate) type AnsibleDocModuleMap = BTreeMap<String, String>;

/// Run `ansible-doc --list --json` against the user's real
/// `ansible-doc` binary.
///
/// The `binary` argument is always `"ansible-doc"` in production and
/// exists as a test seam: subprocess-failure tests inject a
/// nonexistent path so the spawn-time "file not found" path is
/// exercised without mutating `$PATH`.
pub(crate) async fn run_ansible_doc_list_with_binary(
    cwd: &Path,
    binary: &str,
) -> Option<AnsibleDocModuleMap> {
    let output = match tokio::time::timeout(
        Duration::from_millis(ANSIBLE_DOC_LIST_TIMEOUT_MS),
        Command::new(binary)
            .args(["--list", "--json"])
            .current_dir(cwd)
            .kill_on_drop(true)
            .output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::warn!("ansible-doc --list --json command failed: {e}");
            return None;
        }
        Err(_) => {
            tracing::warn!(
                "ansible-doc --list --json timed out after {ANSIBLE_DOC_LIST_TIMEOUT_MS}ms"
            );
            return None;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            exit = ?output.status.code(),
            stderr = %stderr.trim(),
            "ansible-doc --list --json failed"
        );
        return None;
    }

    match serde_json::from_slice::<AnsibleDocModuleMap>(&output.stdout) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("ansible-doc --list --json parse failed: {e}");
            None
        }
    }
}

/// Pure parsing step — exposed only to tests so they can exercise the
/// malformed-JSON branch without spawning a subprocess.
#[cfg(test)]
fn parse_ansible_doc_raw(json: &str) -> Option<AnsibleDocModuleMap> {
    serde_json::from_str(json).ok()
}

/// Project the module map into suggestions. Pure so tests can cover
/// the transformation without a subprocess. `BTreeMap`'s iteration
/// order is alphabetical by key, which is the order the suggestions
/// land in.
fn modules_to_suggestions(modules: AnsibleDocModuleMap) -> Vec<Suggestion> {
    modules
        .into_iter()
        .map(|(name, description)| Suggestion {
            text: name,
            description: Some(description),
            kind: SuggestionKind::ProviderValue,
            source: SuggestionSource::Provider,
            ..Default::default()
        })
        .collect()
}

/// `ansible-doc --list --json` — enumerates every ansible module
/// available on the host. Replaces the `requires_js: true` generator
/// in `specs/ansible-doc.json` whose JS source projected the keys out
/// of the top-level JSON object.
pub struct AnsibleDocModules;

impl Provider for AnsibleDocModules {
    fn name(&self) -> &'static str {
        "ansible_doc_modules"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "ansible-doc").await
    }
}

impl AnsibleDocModules {
    /// Test seam — `generate` calls this with `"ansible-doc"`; tests
    /// call it with a nonexistent path to exercise the
    /// spawn-failure → `Ok(vec![])` fallback contract without mutating
    /// `$PATH`.
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        let Some(modules) = run_ansible_doc_list_with_binary(&ctx.cwd, binary).await else {
            return Ok(Vec::new());
        };
        Ok(modules_to_suggestions(modules))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_happy_path() {
        // Three modules across two collections. BTreeMap sorts keys
        // alphabetically, so the assertion order is stable regardless
        // of JSON declaration order.
        let json = r#"{
            "ansible.builtin.copy": "Copy files to remote locations",
            "ansible.builtin.apt": "Manages apt-packages",
            "community.general.homebrew": "Package manager for Homebrew"
        }"#;
        let modules = parse_ansible_doc_raw(json).expect("parse should succeed");
        let suggestions = modules_to_suggestions(modules);
        assert_eq!(suggestions.len(), 3);
        assert_eq!(suggestions[0].text, "ansible.builtin.apt");
        assert_eq!(
            suggestions[0].description.as_deref(),
            Some("Manages apt-packages")
        );
        assert_eq!(suggestions[1].text, "ansible.builtin.copy");
        assert_eq!(
            suggestions[1].description.as_deref(),
            Some("Copy files to remote locations")
        );
        assert_eq!(suggestions[2].text, "community.general.homebrew");
        assert_eq!(
            suggestions[2].description.as_deref(),
            Some("Package manager for Homebrew")
        );
        for s in &suggestions {
            assert_eq!(s.kind, SuggestionKind::ProviderValue);
            assert_eq!(s.source, SuggestionSource::Provider);
        }
    }

    #[test]
    fn parse_empty_object() {
        // `{}` is a valid wire shape — zero modules — and must round
        // trip to an empty Vec without panic.
        let modules = parse_ansible_doc_raw("{}").expect("parse should succeed");
        assert!(modules.is_empty());
        assert!(modules_to_suggestions(modules).is_empty());
    }

    #[test]
    fn parse_malformed_json_returns_none() {
        // Non-JSON and array-shaped inputs both fall into the None
        // branch. The array case guards against older ansible-doc
        // versions (or unrelated commands) that might emit a different
        // top-level shape — we return None rather than misinterpret.
        assert!(parse_ansible_doc_raw("not json").is_none());
        assert!(parse_ansible_doc_raw("[]").is_none());
        assert!(parse_ansible_doc_raw("").is_none());
        assert!(parse_ansible_doc_raw("{").is_none());
    }

    #[test]
    fn parse_description_with_special_chars() {
        // Descriptions are free-form strings copied verbatim from
        // module docstrings — commas, quotes, parens, and unicode all
        // appear in the wild. The pipeline must pass them through
        // without mangling.
        let json = r#"{
            "ansible.builtin.debug": "Print statements during execution, with \"pretty\" quoting",
            "community.general.unicode": "Manages résumé, naïve, and emoji payloads"
        }"#;
        let modules = parse_ansible_doc_raw(json).expect("parse should succeed");
        let suggestions = modules_to_suggestions(modules);
        assert_eq!(suggestions.len(), 2);
        assert_eq!(suggestions[0].text, "ansible.builtin.debug");
        assert_eq!(
            suggestions[0].description.as_deref(),
            Some("Print statements during execution, with \"pretty\" quoting")
        );
        assert_eq!(suggestions[1].text, "community.general.unicode");
        assert_eq!(
            suggestions[1].description.as_deref(),
            Some("Manages résumé, naïve, and emoji payloads")
        );
    }

    #[tokio::test]
    async fn subprocess_failure_returns_none() {
        // Exercises the spawn-time "file not found" path by injecting
        // a binary name that cannot resolve anywhere on disk. No
        // global state mutated — safe to run in parallel.
        let tmp = tempfile::TempDir::new().unwrap();
        let result = run_ansible_doc_list_with_binary(
            tmp.path(),
            "/nonexistent/ansible-doc-definitely-not-real",
        )
        .await;
        assert!(
            result.is_none(),
            "expected None when the ansible-doc binary does not exist"
        );
    }

    #[tokio::test]
    async fn generate_returns_ok_empty_when_binary_missing() {
        // End-to-end: `AnsibleDocModules::generate` MUST translate a
        // spawn-time failure into `Ok(vec![])`. A silent shift from
        // `Ok` to `Err` would stall the completion pipeline — the
        // engine's warn+empty-vec wrapper would log the error but the
        // bug would survive until someone reads the logs.
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ProviderCtx {
            cwd: tmp.path().to_path_buf(),
            env: std::sync::Arc::new(std::collections::HashMap::new()),
            current_token: String::new(),
        };
        let result = AnsibleDocModules
            .generate_with_binary(&ctx, "/nonexistent/ansible-doc-for-test")
            .await;
        assert!(matches!(result, Ok(ref v) if v.is_empty()));
    }
}
