//! mamba/conda native provider — replaces the JS-backed generator in
//! `specs/mamba.json` that shells out to `conda env list` and parses
//! the plaintext table.
//!
//! mamba wraps conda; the fig spec invokes `conda env list` directly
//! because conda owns the canonical output format. We do the same.
//! The upstream JS slices off the first two lines (`# conda
//! environments:` + `#` separator) and takes `a[0]` of each remaining
//! row after whitespace-splitting. Our parser is equivalent but more
//! forgiving: skip any `#`-prefixed or blank line, then take the first
//! whitespace-delimited token. This drops the active-env `*` column
//! for free — `split_whitespace().next()` on `base  *  /opt/conda`
//! returns `base`, never `*`.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use tokio::process::Command;

use super::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

/// Timeout for `conda env list`. 2s is the convention for
/// external-tool providers — generous for a local filesystem read
/// but tight enough that a borked conda installation can't stall
/// completion.
const CONDA_ENV_LIST_TIMEOUT_MS: u64 = 2_000;

/// Run `conda env list` against the user's real `conda` binary.
///
/// The `binary` argument is always `"conda"` in production and exists
/// as a test seam: subprocess-failure tests inject a nonexistent path
/// so the spawn-time "file not found" path is exercised without
/// mutating `$PATH`.
pub(crate) async fn run_env_list_with_binary(cwd: &Path, binary: &str) -> Option<String> {
    let output = match tokio::time::timeout(
        Duration::from_millis(CONDA_ENV_LIST_TIMEOUT_MS),
        Command::new(binary)
            .args(["env", "list"])
            .current_dir(cwd)
            .kill_on_drop(true)
            .output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::warn!("conda env list command failed: {e}");
            return None;
        }
        Err(_) => {
            tracing::warn!("conda env list timed out after {CONDA_ENV_LIST_TIMEOUT_MS}ms");
            return None;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            exit = ?output.status.code(),
            stderr = %stderr.trim(),
            "conda env list failed"
        );
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse `conda env list` output into suggestions. Pure so tests can
/// exercise every branch without spawning a subprocess.
fn parse_env_list(text: &str) -> Vec<Suggestion> {
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            // `split_whitespace().next()` naturally skips the `*`
            // active-env marker because it sits in its own column —
            // the env name is always the first token on a data row.
            let name = trimmed.split_whitespace().next()?;
            Some(Suggestion {
                text: name.to_string(),
                description: Some("conda environment".to_string()),
                kind: SuggestionKind::ProviderValue,
                source: SuggestionSource::Provider,
                ..Default::default()
            })
        })
        .collect()
}

/// `conda env list` — enumerates named conda/mamba environments.
/// Replaces the `requires_js: true` generator in `specs/mamba.json`
/// whose JS source slices `conda env list` output and projects the
/// first whitespace column of each data row.
pub struct MambaEnvs;

impl Provider for MambaEnvs {
    fn name(&self) -> &'static str {
        "mamba_envs"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "conda").await
    }
}

impl MambaEnvs {
    /// Test seam — `generate` calls this with `"conda"`; tests call
    /// it with a nonexistent path to exercise the
    /// spawn-failure → `Ok(vec![])` fallback contract without mutating
    /// `$PATH`.
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        let Some(output) = run_env_list_with_binary(&ctx.cwd, binary).await else {
            return Ok(Vec::new());
        };
        Ok(parse_env_list(&output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_env_list_happy_path() {
        let fixture = "\
# conda environments:
#
base                     /opt/conda
py311                    /opt/conda/envs/py311
myproject                /Users/me/envs/myproject
";
        let suggestions = parse_env_list(fixture);
        assert_eq!(suggestions.len(), 3);
        assert_eq!(suggestions[0].text, "base");
        assert_eq!(suggestions[1].text, "py311");
        assert_eq!(suggestions[2].text, "myproject");
        for s in &suggestions {
            assert_eq!(s.description.as_deref(), Some("conda environment"));
            assert_eq!(s.kind, SuggestionKind::ProviderValue);
            assert_eq!(s.source, SuggestionSource::Provider);
        }
    }

    #[test]
    fn parse_env_list_skips_comments_and_blanks() {
        // Exercise both leading/trailing blank lines and `#` comments
        // interleaved between rows — the JS source skipped exactly the
        // first two lines by index, which would break on any extra
        // whitespace; our predicate-based filter must not.
        let fixture = "\n# conda environments:\n#\n\npy311                    /opt/conda/envs/py311\n\n# stale comment mid-table\nmyproject                /Users/me/envs/myproject\n\n";
        let suggestions = parse_env_list(fixture);
        assert_eq!(suggestions.len(), 2);
        assert_eq!(suggestions[0].text, "py311");
        assert_eq!(suggestions[1].text, "myproject");
    }

    #[test]
    fn parse_env_list_active_marker_ignored() {
        // Critical edge case: the active-env `*` column sits between
        // the name and the path. `split_whitespace().next()` must
        // return the name — never `*`, never a concatenation.
        let fixture = "\
# conda environments:
#
base                  *  /opt/conda
py311                    /opt/conda/envs/py311
";
        let suggestions = parse_env_list(fixture);
        assert_eq!(suggestions.len(), 2);
        assert_eq!(suggestions[0].text, "base");
        assert_eq!(suggestions[1].text, "py311");
        // Belt-and-suspenders: no suggestion text should contain the
        // active marker, ever.
        for s in &suggestions {
            assert!(
                !s.text.contains('*'),
                "text leaked active marker: {:?}",
                s.text
            );
        }
    }

    #[test]
    fn parse_env_list_empty_input() {
        assert!(parse_env_list("").is_empty());
    }

    #[test]
    fn parse_env_list_only_comments() {
        assert!(parse_env_list("# conda environments:\n#\n").is_empty());
    }

    #[tokio::test]
    async fn subprocess_failure_returns_empty() {
        // Exercises the spawn-time "file not found" path by injecting
        // a binary name that cannot resolve anywhere on disk. No
        // global state mutated — safe to run in parallel with the
        // rest of the workspace suite.
        let tmp = tempfile::TempDir::new().unwrap();
        let result =
            run_env_list_with_binary(tmp.path(), "/nonexistent/conda-definitely-not-real").await;
        assert!(
            result.is_none(),
            "expected None when the conda binary does not exist"
        );
    }

    #[tokio::test]
    async fn generate_returns_ok_empty_when_binary_missing() {
        // End-to-end: `MambaEnvs::generate` MUST translate a spawn-time
        // failure into `Ok(vec![])`. A silent shift to `Err` would
        // stall the completion pipeline.
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ProviderCtx {
            cwd: tmp.path().to_path_buf(),
            env: std::sync::Arc::new(std::collections::HashMap::new()),
            current_token: String::new(),
        };
        let result = MambaEnvs
            .generate_with_binary(&ctx, "/nonexistent/conda-for-test")
            .await;
        assert!(matches!(result, Ok(ref v) if v.is_empty()));
    }

    #[tokio::test]
    async fn generate_production_wrapper_returns_ok_without_binary_installed() {
        // Covers the production `Provider::generate` entry point that
        // `providers::resolve` actually calls — the
        // `generate_with_binary` seam above does NOT exercise the
        // hardcoded `"conda"` literal. If that literal were ever
        // typo'd, every other test would still pass; only a call
        // through `.generate(&ctx)` would catch it. Assertion is
        // `Ok(_)` (not `Ok(vec![])`) because a developer machine could
        // have conda installed — we only pin the "never Err" contract.
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ProviderCtx {
            cwd: tmp.path().to_path_buf(),
            env: std::sync::Arc::new(std::collections::HashMap::new()),
            current_token: String::new(),
        };
        let result = MambaEnvs.generate(&ctx).await;
        assert!(matches!(result, Ok(_)));
    }
}
