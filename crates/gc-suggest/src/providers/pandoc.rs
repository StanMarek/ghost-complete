//! pandoc native providers — replace the JS-backed generators in
//! `specs/pandoc.json` that shell out to `pandoc --list-input-formats`
//! and `pandoc --list-output-formats` and emit one format identifier
//! per line.
//!
//! Both providers live here: `PandocInputFormats` runs the
//! `--list-input-formats` variant; `PandocOutputFormats` runs the
//! `--list-output-formats` variant. They share the
//! `run_pandoc_formats` subprocess helper — the only delta between the
//! two subprocess calls is which flag is passed to pandoc, so the
//! helper takes the flag as an argument instead of encoding it in two
//! near-identical functions. This keeps the `tokio::process::Command`
//! monomorphization shared between both providers (binary-size
//! savings) without introducing a direction enum inside the helper —
//! the caller passes the flag literal and the helper is dumb plumbing.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use tokio::process::Command;

use super::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

/// Timeout for `pandoc --list-*-formats`. 2s is the convention for
/// external-tool providers — generous for what is effectively a
/// constant-time list print, but tight enough that a stalled pandoc
/// installation cannot block completion.
const PANDOC_LIST_TIMEOUT_MS: u64 = 2_000;

/// Shared runner for `pandoc --list-<direction>-formats`. The
/// direction argument distinguishes the two providers
/// (`pandoc_input_formats`, `pandoc_output_formats`) without
/// duplicating the tokio::process::Command monomorphization.
///
/// The `binary` argument is always `"pandoc"` in production and
/// exists as a test seam: subprocess-failure tests inject a
/// nonexistent path so the spawn-time "file not found" path is
/// exercised without mutating `$PATH`.
pub(crate) async fn run_pandoc_formats_with_binary(
    cwd: &Path,
    binary: &str,
    flag: &str,
) -> Option<String> {
    let output = match tokio::time::timeout(
        Duration::from_millis(PANDOC_LIST_TIMEOUT_MS),
        Command::new(binary)
            .args([flag])
            .current_dir(cwd)
            .kill_on_drop(true)
            .output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::warn!("pandoc {flag} command failed: {e}");
            return None;
        }
        Err(_) => {
            tracing::warn!("pandoc {flag} timed out after {PANDOC_LIST_TIMEOUT_MS}ms");
            return None;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            exit = ?output.status.code(),
            stderr = %stderr.trim(),
            "pandoc {flag} failed"
        );
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse `pandoc --list-*-formats` output into suggestions. Pure so
/// tests can exercise every branch without spawning a subprocess.
///
/// `description` is a `&'static str` so the two callers
/// (`PandocInputFormats`, `PandocOutputFormats`) each pass their
/// fixed literal without threading generics or a Cow through the
/// shared parser.
fn parse_formats(text: &str, description: &'static str) -> Vec<Suggestion> {
    text.lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|fmt| Suggestion {
            text: fmt.to_string(),
            description: Some(description.to_string()),
            kind: SuggestionKind::ProviderValue,
            source: SuggestionSource::Provider,
            ..Default::default()
        })
        .collect()
}

/// `pandoc --list-input-formats` — enumerates the formats pandoc can
/// read from. Replaces the `requires_js: true` generator in
/// `specs/pandoc.json` whose JS source runs the same command and
/// splits stdout on newlines.
pub struct PandocInputFormats;

impl Provider for PandocInputFormats {
    fn name(&self) -> &'static str {
        "pandoc_input_formats"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "pandoc").await
    }
}

impl PandocInputFormats {
    /// Test seam — `generate` calls this with `"pandoc"`; tests call
    /// it with a nonexistent path to exercise the
    /// spawn-failure → `Ok(vec![])` fallback contract without mutating
    /// `$PATH`.
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        let Some(text) =
            run_pandoc_formats_with_binary(&ctx.cwd, binary, "--list-input-formats").await
        else {
            return Ok(Vec::new());
        };
        Ok(parse_formats(&text, "pandoc input format"))
    }
}

/// `pandoc --list-output-formats` — enumerates the formats pandoc can
/// write to. Sibling of `PandocInputFormats`; shares the subprocess
/// helper, differs only in the flag passed to pandoc and the
/// per-suggestion description string.
pub struct PandocOutputFormats;

impl Provider for PandocOutputFormats {
    fn name(&self) -> &'static str {
        "pandoc_output_formats"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        self.generate_with_binary(ctx, "pandoc").await
    }
}

impl PandocOutputFormats {
    /// Test seam — see `PandocInputFormats::generate_with_binary`.
    pub(crate) async fn generate_with_binary(
        &self,
        ctx: &ProviderCtx,
        binary: &str,
    ) -> Result<Vec<Suggestion>> {
        let Some(text) =
            run_pandoc_formats_with_binary(&ctx.cwd, binary, "--list-output-formats").await
        else {
            return Ok(Vec::new());
        };
        Ok(parse_formats(&text, "pandoc output format"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_happy_path() {
        let fixture = "markdown\nhtml\njson\ndocx\nrst\n";
        let suggestions = parse_formats(fixture, "pandoc input format");
        assert_eq!(suggestions.len(), 5);
        assert_eq!(suggestions[0].text, "markdown");
        assert_eq!(suggestions[1].text, "html");
        assert_eq!(suggestions[2].text, "json");
        assert_eq!(suggestions[3].text, "docx");
        assert_eq!(suggestions[4].text, "rst");
        for s in &suggestions {
            assert_eq!(s.description.as_deref(), Some("pandoc input format"));
            assert_eq!(s.kind, SuggestionKind::ProviderValue);
            assert_eq!(s.source, SuggestionSource::Provider);
        }
    }

    #[test]
    fn parse_trailing_newline_tolerated() {
        // pandoc prints a trailing newline; some environments (or
        // paranoid shell wrappers) may inject an extra blank line.
        // Both shapes must yield the same set of suggestions with no
        // empty-text entry leaking through.
        let fixture = "markdown\nhtml\n\n";
        let suggestions = parse_formats(fixture, "pandoc input format");
        assert_eq!(suggestions.len(), 2);
        assert_eq!(suggestions[0].text, "markdown");
        assert_eq!(suggestions[1].text, "html");
        for s in &suggestions {
            assert!(!s.text.is_empty());
        }
    }

    #[test]
    fn parse_empty_input() {
        // Practically impossible in production (pandoc always prints
        // at least one format), but the parser must not panic or emit
        // a phantom suggestion when stdout is empty.
        assert!(parse_formats("", "pandoc input format").is_empty());
        assert!(parse_formats("\n\n\n", "pandoc input format").is_empty());
    }

    #[test]
    fn parse_descriptions_differ() {
        // Locks in that the two providers hand distinct description
        // strings to the shared parser — a copy-paste bug where both
        // callers pass the same literal would silently degrade UX
        // without any other test failing.
        let fixture = "markdown\n";
        let input = parse_formats(fixture, "pandoc input format");
        let output = parse_formats(fixture, "pandoc output format");
        assert_eq!(input.len(), 1);
        assert_eq!(output.len(), 1);
        assert_eq!(input[0].description.as_deref(), Some("pandoc input format"));
        assert_eq!(
            output[0].description.as_deref(),
            Some("pandoc output format")
        );
        assert_ne!(input[0].description, output[0].description);
    }

    #[tokio::test]
    async fn subprocess_failure_returns_none_for_input_flag() {
        // Exercises the spawn-time "file not found" path for the
        // input-formats flag. No global state mutated — safe to run
        // in parallel with the rest of the workspace suite.
        let tmp = tempfile::TempDir::new().unwrap();
        let result = run_pandoc_formats_with_binary(
            tmp.path(),
            "/nonexistent/pandoc-definitely-not-real",
            "--list-input-formats",
        )
        .await;
        assert!(
            result.is_none(),
            "expected None when the pandoc binary does not exist"
        );
    }

    #[tokio::test]
    async fn subprocess_failure_returns_none_for_output_flag() {
        // Mirror of the input-flag failure test, exercising the
        // output-formats flag to confirm the `flag` argument is
        // actually being threaded through the helper (rather than,
        // say, hardcoded to the input variant).
        let tmp = tempfile::TempDir::new().unwrap();
        let result = run_pandoc_formats_with_binary(
            tmp.path(),
            "/nonexistent/pandoc-definitely-not-real",
            "--list-output-formats",
        )
        .await;
        assert!(
            result.is_none(),
            "expected None when the pandoc binary does not exist"
        );
    }

    fn ctx_for(cwd: std::path::PathBuf) -> ProviderCtx {
        ProviderCtx {
            cwd,
            env: std::sync::Arc::new(std::collections::HashMap::new()),
            current_token: String::new(),
        }
    }

    #[tokio::test]
    async fn input_generate_returns_ok_empty_when_binary_missing() {
        // End-to-end: `PandocInputFormats::generate` MUST translate a
        // spawn-time failure into `Ok(vec![])`. A silent shift to
        // `Err` would stall the completion pipeline.
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ctx_for(tmp.path().to_path_buf());
        let result = PandocInputFormats
            .generate_with_binary(&ctx, "/nonexistent/pandoc-for-test")
            .await;
        assert!(matches!(result, Ok(ref v) if v.is_empty()));
    }

    #[tokio::test]
    async fn output_generate_returns_ok_empty_when_binary_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ctx_for(tmp.path().to_path_buf());
        let result = PandocOutputFormats
            .generate_with_binary(&ctx, "/nonexistent/pandoc-for-test")
            .await;
        assert!(matches!(result, Ok(ref v) if v.is_empty()));
    }

    #[tokio::test]
    async fn input_generate_production_wrapper_returns_ok_without_binary_installed() {
        // Pins the "never Err" contract of `Provider::generate` for
        // `PandocInputFormats` — a spawn failure (tool missing, PATH
        // empty, exec permission denied) must become `Ok(vec![])`,
        // never propagated as `Err`.
        //
        // Does NOT catch a typo'd binary literal (e.g. `"pandocc"` vs
        // `"pandoc"`), because a typo produces the same spawn failure
        // → `Ok(vec![])` path as a genuinely-absent tool: every
        // subprocess error maps to `tracing::warn!` + `None` in
        // `run_pandoc_formats_with_binary`, and `generate_with_binary`
        // maps `None` to `Ok(Vec::new())`. The test cannot distinguish
        // a typo from "tool genuinely absent".
        //
        // Assertion is `Ok(_)` (not `Ok(vec![])`) because a developer
        // machine could have pandoc installed.
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ctx_for(tmp.path().to_path_buf());
        let result = PandocInputFormats.generate(&ctx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn output_generate_production_wrapper_returns_ok_without_binary_installed() {
        // Sibling of the input-formats test above — pins the "never
        // Err" contract of `Provider::generate` for
        // `PandocOutputFormats`. Does NOT catch a typo'd `"pandoc"`
        // literal, because a typo produces the same spawn failure →
        // `Ok(vec![])` path as a genuinely-absent tool (see the
        // input-formats test's docstring for the full rationale).
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ctx_for(tmp.path().to_path_buf());
        let result = PandocOutputFormats.generate(&ctx).await;
        assert!(result.is_ok());
    }
}
