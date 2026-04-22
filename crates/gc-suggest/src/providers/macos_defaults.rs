//! macOS `defaults` native provider — replaces the JS-backed generator
//! in `specs/defaults.json` that shells out to `defaults domains` and
//! splits the single-line, comma-separated preference domain list into
//! discrete completions.
//!
//! `defaults domains` is macOS-only: the binary ships with the OS and
//! has no Linux counterpart. On Linux the subprocess fails at spawn
//! time, which is indistinguishable from "tool not installed" and flows
//! through the standard `None` → empty-Vec path used by every other
//! Phase 3A provider.
//!
//! Filename is `macos_defaults.rs` rather than `defaults.rs` so the
//! module name does not collide visually with Rust's ubiquitous
//! `Default` trait.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use tokio::process::Command;

use super::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

/// Timeout for `defaults domains`. 2s matches the Phase 3A default for
/// external-tool subprocesses — generous for an on-device preference
/// read but tight enough that a stuck `cfprefsd` can't stall completion.
const DEFAULTS_DOMAINS_TIMEOUT_MS: u64 = 2_000;

/// Run `defaults domains` against the user's real `defaults` binary.
pub(crate) async fn run_defaults_domains(cwd: &Path) -> Option<String> {
    run_defaults_domains_with_binary(cwd, "defaults").await
}

// Parametric binary name lets subprocess-failure tests inject a
// nonexistent path without mutating $PATH. Production callers go
// through `run_defaults_domains`.
pub(crate) async fn run_defaults_domains_with_binary(cwd: &Path, binary: &str) -> Option<String> {
    let output = match tokio::time::timeout(
        Duration::from_millis(DEFAULTS_DOMAINS_TIMEOUT_MS),
        Command::new(binary)
            .args(["domains"])
            .current_dir(cwd)
            .kill_on_drop(true)
            .output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::warn!("defaults domains command failed: {e}");
            return None;
        }
        Err(_) => {
            tracing::warn!("defaults domains timed out after {DEFAULTS_DOMAINS_TIMEOUT_MS}ms");
            return None;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            exit = ?output.status.code(),
            stderr = %stderr.trim(),
            "defaults domains failed"
        );
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse `defaults domains` output into suggestions. Pure so tests can
/// exercise every branch without spawning a subprocess.
///
/// The wire shape is a single line of comma-space-separated domain
/// identifiers, optionally with a trailing comma and/or trailing
/// newline. `split(',')` + `trim` handles all three: extra whitespace
/// around each token, a dangling empty segment after a trailing comma,
/// and the terminal `\n`.
fn parse_domains(text: &str) -> Vec<Suggestion> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|domain| Suggestion {
            text: domain.to_string(),
            description: Some("defaults domain".to_string()),
            kind: SuggestionKind::Command,
            source: SuggestionSource::Provider,
            ..Default::default()
        })
        .collect()
}

/// `defaults domains` — enumerates macOS preference domains. Replaces
/// the `requires_js: true` generator in `specs/defaults.json` whose JS
/// source split the single-line output on `, ` and dropped the trailing
/// empty token.
pub struct DefaultsDomains;

impl Provider for DefaultsDomains {
    fn name(&self) -> &'static str {
        "defaults_domains"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        let Some(output) = run_defaults_domains(&ctx.cwd).await else {
            return Ok(Vec::new());
        };
        Ok(parse_domains(&output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_happy_path() {
        // Canonical wire shape: comma-space separated, trailing newline,
        // no trailing comma. Every token must round-trip to a
        // `Suggestion` with the expected metadata.
        let fixture = "com.apple.dock, com.apple.finder, com.apple.Safari, com.googlecode.iterm2\n";
        let suggestions = parse_domains(fixture);
        assert_eq!(suggestions.len(), 4);
        assert_eq!(suggestions[0].text, "com.apple.dock");
        assert_eq!(suggestions[1].text, "com.apple.finder");
        assert_eq!(suggestions[2].text, "com.apple.Safari");
        assert_eq!(suggestions[3].text, "com.googlecode.iterm2");
        for s in &suggestions {
            assert_eq!(s.description.as_deref(), Some("defaults domain"));
            assert_eq!(s.kind, SuggestionKind::Command);
            assert_eq!(s.source, SuggestionSource::Provider);
        }
    }

    #[test]
    fn parse_trailing_comma_tolerated() {
        // Some macOS versions emit `a, b, c,` with a dangling comma.
        // `split(',')` produces an empty final segment in that case —
        // the `filter(!is_empty)` arm must drop it rather than producing
        // a nameless suggestion.
        let fixture = "com.apple.dock, com.apple.finder, com.apple.Safari,\n";
        let suggestions = parse_domains(fixture);
        assert_eq!(suggestions.len(), 3);
        assert_eq!(suggestions[0].text, "com.apple.dock");
        assert_eq!(suggestions[1].text, "com.apple.finder");
        assert_eq!(suggestions[2].text, "com.apple.Safari");
        for s in &suggestions {
            assert!(
                !s.text.is_empty(),
                "trailing comma produced an empty suggestion"
            );
        }
    }

    #[test]
    fn parse_extra_whitespace() {
        // Irregular spacing around separators — `defaults` normally
        // emits a single space after each comma, but we must tolerate
        // any amount of leading/trailing whitespace per token so a
        // future OS-level formatting change can't break us.
        let fixture = "a,  b , c";
        let suggestions = parse_domains(fixture);
        assert_eq!(suggestions.len(), 3);
        assert_eq!(suggestions[0].text, "a");
        assert_eq!(suggestions[1].text, "b");
        assert_eq!(suggestions[2].text, "c");
    }

    #[test]
    fn parse_empty_input() {
        // Both empty-string and bare-newline inputs must yield an empty
        // Vec — the newline-only case is what `defaults domains` emits
        // when no domains are registered (vanishingly rare in practice
        // but cheap to cover).
        assert!(parse_domains("").is_empty());
        assert!(parse_domains("\n").is_empty());
    }

    #[tokio::test]
    async fn subprocess_failure_returns_none() {
        // Exercises the spawn-time "file not found" path by injecting
        // a binary name that cannot resolve anywhere on disk. No
        // global state mutated — safe to run in parallel with the
        // rest of the workspace suite, and also exercises the Linux
        // code path where `defaults` genuinely does not exist.
        let tmp = tempfile::TempDir::new().unwrap();
        let result = run_defaults_domains_with_binary(
            tmp.path(),
            "/nonexistent/defaults-definitely-not-real",
        )
        .await;
        assert!(
            result.is_none(),
            "expected None when the defaults binary does not exist"
        );
    }
}
