//! `NpmScripts` provider — keys of the nearest ancestor
//! `package.json#scripts` object. Description is the script's command
//! string, truncated to 120 characters at a UTF-8 char boundary.
//!
//! Does not honour `package.json#fig.scripts` overrides — that's a v2
//! concern. yarn / pnpm share this same parser when their providers
//! come online.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::Result;
use serde::Deserialize;

use super::{MtimeCache, MAX_ANCESTOR_WALK};
use crate::providers::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

/// Maximum description length, in characters (NOT bytes). 120 keeps
/// the popup readable on a typical 80–120 column terminal.
const MAX_DESCRIPTION_CHARS: usize = 120;

#[derive(Deserialize)]
struct PackageJsonMin {
    #[serde(default)]
    scripts: serde_json::Map<String, serde_json::Value>,
}

/// One entry in `package.json#scripts`. Field-named so the cache and
/// `Suggestion` build site can't accidentally swap name and command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NpmScriptEntry {
    pub name: String,
    pub command: Option<String>,
}

static NPM_CACHE: LazyLock<MtimeCache<Vec<NpmScriptEntry>>> = LazyLock::new(MtimeCache::new);

/// Parse `package.json` bytes and yield `NpmScriptEntry` values in
/// source order. Non-string script values are dropped (npm itself
/// rejects them at runtime) but logged so a malformed entry surfaces.
pub(crate) fn parse_npm_scripts(bytes: &[u8]) -> Vec<NpmScriptEntry> {
    let parsed: PackageJsonMin = match serde_json::from_slice(bytes) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "package.json parse failed");
            return Vec::new();
        }
    };

    parsed
        .scripts
        .into_iter()
        .filter_map(|(name, value)| match value {
            serde_json::Value::String(cmd) => Some(NpmScriptEntry {
                name,
                command: Some(truncate_chars(&cmd)),
            }),
            _ => {
                tracing::warn!(
                    script = %name,
                    "package.json scripts.{name} is not a string; npm run will reject it too — skipping"
                );
                None
            }
        })
        .collect()
}

fn truncate_chars(s: &str) -> String {
    if s.chars().count() <= MAX_DESCRIPTION_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX_DESCRIPTION_CHARS - 1).collect();
    out.push('…');
    out
}

/// Walk ancestors of `start` looking for `package.json`. First hit
/// wins.
pub(crate) fn find_package_json(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    for _ in 0..MAX_ANCESTOR_WALK {
        let dir = current?;
        let candidate = dir.join("package.json");
        if candidate.is_file() {
            return Some(candidate);
        }
        current = dir.parent();
    }
    None
}

pub struct NpmScripts;

impl Provider for NpmScripts {
    fn name(&self) -> &'static str {
        "npm_scripts"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        Self::generate_with_root(&ctx.cwd).await
    }
}

impl NpmScripts {
    pub(crate) async fn generate_with_root(root: &Path) -> Result<Vec<Suggestion>> {
        let Some(path) = find_package_json(root) else {
            return Ok(Vec::new());
        };
        let Some(scripts) = NPM_CACHE.get_or_insert_with(&path, parse_npm_scripts) else {
            return Ok(Vec::new());
        };
        Ok(scripts
            .into_iter()
            .map(|entry| Suggestion {
                text: entry.name,
                description: entry.command,
                kind: SuggestionKind::ProviderValue,
                source: SuggestionSource::Provider,
                ..Default::default()
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn entry(name: &str, cmd: &str) -> NpmScriptEntry {
        NpmScriptEntry {
            name: name.to_string(),
            command: Some(cmd.to_string()),
        }
    }

    #[test]
    fn happy_path_two_scripts() {
        let json = br#"{"scripts": {"build": "tsc", "test": "jest"}}"#;
        let scripts = parse_npm_scripts(json);
        assert_eq!(scripts, vec![entry("build", "tsc"), entry("test", "jest")]);
    }

    #[test]
    fn empty_scripts_object_returns_empty() {
        assert!(parse_npm_scripts(br#"{"scripts": {}}"#).is_empty());
    }

    #[test]
    fn missing_scripts_field_returns_empty() {
        assert!(parse_npm_scripts(br#"{"name": "pkg"}"#).is_empty());
    }

    #[test]
    fn non_string_values_skipped() {
        let json = br#"{"scripts": {"x": 42, "y": "echo y", "z": null}}"#;
        let scripts = parse_npm_scripts(json);
        assert_eq!(scripts, vec![entry("y", "echo y")]);
    }

    #[test]
    fn scripts_field_as_string_returns_empty() {
        // `"scripts": "oops"` — the deserialize fails because we expect
        // a Map; the warn-and-empty path must surface no panic.
        assert!(parse_npm_scripts(br#"{"scripts": "oops"}"#).is_empty());
    }

    #[test]
    fn scripts_field_as_null_returns_empty() {
        // `"scripts": null` is the default value via `#[serde(default)]`
        // — the Map deserializer doesn't accept null directly, so we
        // assert no-panic + empty rather than asserting which arm wins.
        assert!(parse_npm_scripts(br#"{"scripts": null}"#).is_empty());
    }

    #[test]
    fn scripts_field_as_array_returns_empty() {
        assert!(parse_npm_scripts(br#"{"scripts": ["start"]}"#).is_empty());
    }

    #[test]
    fn description_truncated_at_120_chars() {
        let long = "a".repeat(200);
        let json = format!(r#"{{"scripts": {{"big": "{long}"}}}}"#);
        let scripts = parse_npm_scripts(json.as_bytes());
        let desc = scripts[0].command.as_ref().unwrap();
        assert_eq!(
            desc.chars().count(),
            MAX_DESCRIPTION_CHARS,
            "must be exactly {MAX_DESCRIPTION_CHARS} chars including the ellipsis"
        );
        assert!(desc.ends_with('…'));
    }

    #[test]
    fn truncation_respects_utf8_char_boundaries() {
        // 200 multi-byte chars; naive byte slicing would land mid-char.
        let s = "🦀".repeat(200);
        let json = format!(r#"{{"scripts": {{"crab": "{s}"}}}}"#);
        let scripts = parse_npm_scripts(json.as_bytes());
        let desc = scripts[0].command.as_ref().unwrap();
        assert_eq!(desc.chars().count(), MAX_DESCRIPTION_CHARS);
    }

    #[test]
    fn insertion_order_preserved() {
        // serde_json::Map preserves insertion order ONLY because
        // crates/gc-suggest/Cargo.toml enables the `preserve_order`
        // feature on serde_json — without it, Map is a BTreeMap alias
        // and would sort alphabetically. Locking that contract in here
        // so a future feature-flag flip surfaces as a test failure.
        let json = br#"{"scripts": {"z": "1", "a": "2", "m": "3", "c": "4", "b": "5"}}"#;
        let names: Vec<String> = parse_npm_scripts(json)
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["z", "a", "m", "c", "b"]);
    }

    #[test]
    fn malformed_json_returns_empty() {
        assert!(parse_npm_scripts(b"not json").is_empty());
        assert!(parse_npm_scripts(b"").is_empty());
        assert!(parse_npm_scripts(b"{").is_empty());
    }

    #[tokio::test]
    async fn generate_with_root_against_empty_dir_returns_ok_empty() {
        let tmp = TempDir::new().unwrap();
        let result = NpmScripts::generate_with_root(tmp.path()).await;
        assert!(matches!(result, Ok(ref v) if v.is_empty()));
    }

    #[tokio::test]
    async fn generate_with_root_walks_two_ancestor_levels() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("package.json"),
            br#"{"scripts": {"start": "node x.js"}}"#,
        )
        .unwrap();
        let nested = tmp.path().join("src").join("util");
        std::fs::create_dir_all(&nested).unwrap();
        let suggestions = NpmScripts::generate_with_root(&nested).await.unwrap();
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].text, "start");
        assert_eq!(suggestions[0].description.as_deref(), Some("node x.js"));
        assert_eq!(suggestions[0].kind, SuggestionKind::ProviderValue);
        assert_eq!(suggestions[0].source, SuggestionSource::Provider);
    }
}
