//! `CargoWorkspaceMembers` provider — emits the package names of every
//! workspace member declared in the nearest ancestor `Cargo.toml`. For
//! a single-package crate (no `[workspace]` table found in any
//! ancestor) the provider degrades to emitting the one `package.name`
//! so that `cargo run -p <NAME>` still completes.
//!
//! Glob support is intentionally narrow: literal paths and the
//! one-segment trailing glob `prefix/*`. Anything more exotic (`**`,
//! brace expansion, regex) is logged-and-skipped — the user can `cd
//! <crate-dir>` and run `cargo run` bare as a workaround.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::Result;
use serde::Deserialize;

use super::{MAX_ANCESTOR_WALK, MtimeCache};
use crate::providers::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

const MAX_DESCRIPTION_CHARS: usize = 120;

#[derive(Deserialize)]
struct CargoTomlMin {
    #[serde(default)]
    package: Option<PackageMin>,
    #[serde(default)]
    workspace: Option<WorkspaceMin>,
}

#[derive(Deserialize, Clone)]
struct PackageMin {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize, Default)]
struct WorkspaceMin {
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

/// Cached parsed-and-resolved view of a workspace root's members
/// (including the names extracted from each member's own `Cargo.toml`).
/// Resolution itself is the expensive part — once resolved, member
/// listings are cheap to clone.
#[derive(Clone)]
struct ResolvedRoot {
    members: Vec<MemberInfo>,
}

#[derive(Clone, Debug)]
struct MemberInfo {
    name: String,
    description: Option<String>,
}

static CARGO_CACHE: LazyLock<MtimeCache<ResolvedRoot>> = LazyLock::new(MtimeCache::new);

/// Walk ancestors of `start`. Return the first `Cargo.toml` containing
/// a `[workspace]` table; if none is found, return the nearest
/// `Cargo.toml` so the single-package fallback can still emit the one
/// package name.
pub(crate) fn find_cargo_root(start: &Path) -> Option<PathBuf> {
    let mut nearest: Option<PathBuf> = None;
    let mut current = Some(start);
    for _ in 0..MAX_ANCESTOR_WALK {
        let Some(dir) = current else { break };
        let candidate = dir.join("Cargo.toml");
        if candidate.is_file() {
            if has_workspace_section(&candidate) {
                return Some(candidate);
            }
            if nearest.is_none() {
                nearest = Some(candidate);
            }
        }
        current = dir.parent();
    }
    nearest
}

/// Cheap check: does this `Cargo.toml` declare a `[workspace]` table?
/// We parse the file rather than string-grepping so we don't false-fire
/// on `[workspace]` appearing inside a triple-quoted dependency
/// description or comment.
fn has_workspace_section(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return false;
    };
    matches!(
        toml::from_str::<CargoTomlMin>(text),
        Ok(CargoTomlMin { workspace: Some(_), .. })
    )
}

/// Parse a workspace root and resolve member names. The function is
/// the extractor handed to `MtimeCache::get_or_insert_with`; it
/// receives the bytes of the root manifest only — it must read each
/// member's own `Cargo.toml` itself, anchored at `root_dir`.
fn resolve_workspace(root_dir: &Path, bytes: &[u8]) -> ResolvedRoot {
    let parsed: CargoTomlMin = match std::str::from_utf8(bytes)
        .ok()
        .and_then(|s| toml::from_str::<CargoTomlMin>(s).ok())
    {
        Some(p) => p,
        None => {
            tracing::warn!(
                root = %root_dir.display(),
                "Cargo.toml parse failed",
            );
            return ResolvedRoot {
                members: Vec::new(),
            };
        }
    };

    if let Some(ws) = parsed.workspace {
        let exclude_set: std::collections::HashSet<PathBuf> = ws
            .exclude
            .iter()
            .map(|p| root_dir.join(p))
            .collect();

        let mut members: Vec<MemberInfo> = Vec::new();
        for pattern in &ws.members {
            for member_path in expand_member_pattern(root_dir, pattern) {
                if exclude_set.contains(&member_path) {
                    continue;
                }
                if let Some(info) = read_member_info(&member_path) {
                    members.push(info);
                }
            }
        }
        return ResolvedRoot { members };
    }

    if let Some(pkg) = parsed.package {
        if let Some(info) = MemberInfo::from_package(pkg) {
            return ResolvedRoot {
                members: vec![info],
            };
        }
    }

    ResolvedRoot {
        members: Vec::new(),
    }
}

/// Expand one entry of `[workspace].members` into one or more concrete
/// member directory paths (each containing a `Cargo.toml`).
fn expand_member_pattern(root_dir: &Path, pattern: &str) -> Vec<PathBuf> {
    if let Some(prefix) = pattern.strip_suffix("/*") {
        let prefix_dir = root_dir.join(prefix);
        let entries = match std::fs::read_dir(&prefix_dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    pattern = %pattern,
                    prefix = %prefix_dir.display(),
                    error = %e,
                    "workspace glob: read_dir failed; skipping",
                );
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() && p.join("Cargo.toml").is_file() {
                out.push(p);
            }
        }
        out.sort();
        return out;
    }

    if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
        tracing::warn!(
            pattern = %pattern,
            "workspace glob: unsupported pattern; only literal paths and `prefix/*` are recognized",
        );
        return Vec::new();
    }

    let dir = root_dir.join(pattern);
    if dir.join("Cargo.toml").is_file() {
        vec![dir]
    } else {
        Vec::new()
    }
}

fn read_member_info(member_dir: &Path) -> Option<MemberInfo> {
    let manifest = member_dir.join("Cargo.toml");
    let bytes = std::fs::read(&manifest).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?;
    let parsed: CargoTomlMin = toml::from_str(text).ok()?;
    let pkg = parsed.package?;
    let info = MemberInfo::from_package(pkg);
    if info.is_none() {
        tracing::warn!(
            manifest = %manifest.display(),
            "workspace member has no `package.name`; skipping",
        );
    }
    info
}

impl MemberInfo {
    fn from_package(pkg: PackageMin) -> Option<Self> {
        let name = pkg.name?;
        let description = match (pkg.version, pkg.description) {
            (Some(v), Some(d)) => Some(truncate_chars(&format!("{v} — {d}"))),
            (Some(v), None) => Some(format!("v{v}")),
            (None, Some(d)) => Some(truncate_chars(&d)),
            (None, None) => None,
        };
        Some(Self { name, description })
    }
}

fn truncate_chars(s: &str) -> String {
    if s.chars().count() <= MAX_DESCRIPTION_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX_DESCRIPTION_CHARS - 1).collect();
    out.push('…');
    out
}

pub struct CargoWorkspaceMembers;

impl Provider for CargoWorkspaceMembers {
    fn name(&self) -> &'static str {
        "cargo_workspace_members"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        Self::generate_with_root(&ctx.cwd).await
    }
}

impl CargoWorkspaceMembers {
    pub(crate) async fn generate_with_root(root: &Path) -> Result<Vec<Suggestion>> {
        let Some(manifest) = find_cargo_root(root) else {
            return Ok(Vec::new());
        };
        let manifest_dir = manifest
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let Some(resolved) =
            CARGO_CACHE.get_or_insert_with(&manifest, |bytes| resolve_workspace(&manifest_dir, bytes))
        else {
            return Ok(Vec::new());
        };
        Ok(resolved
            .members
            .into_iter()
            .map(|m| Suggestion {
                text: m.name,
                description: m.description,
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

    fn write_member(root: &Path, rel: &str, name: &str) {
        let dir = root.join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn single_package_crate_emits_one_suggestion() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"solo\"\nversion = \"0.2.0\"\ndescription = \"a crate\"\n",
        )
        .unwrap();
        let suggestions = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].text, "solo");
        assert_eq!(suggestions[0].description.as_deref(), Some("0.2.0 — a crate"));
    }

    #[tokio::test]
    async fn workspace_with_literal_members_emits_each() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"a\", \"b\"]\n",
        )
        .unwrap();
        write_member(tmp.path(), "a", "alpha");
        write_member(tmp.path(), "b", "beta");

        let suggestions = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        let names: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[tokio::test]
    async fn workspace_glob_expansion_discovers_subdirs() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        write_member(tmp.path(), "crates/one", "one");
        write_member(tmp.path(), "crates/two", "two");
        write_member(tmp.path(), "crates/three", "three");

        let suggestions = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        let mut names: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["one", "three", "two"]);
    }

    #[tokio::test]
    async fn workspace_exclude_drops_listed_member() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"crates/skip\"]\n",
        )
        .unwrap();
        write_member(tmp.path(), "crates/keep", "keep");
        write_member(tmp.path(), "crates/skip", "skip");

        let suggestions = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        let names: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(names, vec!["keep"]);
    }

    #[tokio::test]
    async fn member_without_package_name_skipped() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"weird\"]\n",
        )
        .unwrap();
        // Member with no [package] section at all.
        let weird = tmp.path().join("weird");
        std::fs::create_dir_all(&weird).unwrap();
        std::fs::write(weird.join("Cargo.toml"), "[lib]\nname = \"x\"\n").unwrap();

        let suggestions = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        assert!(suggestions.is_empty());
    }

    #[tokio::test]
    async fn glob_root_missing_logs_and_returns_empty() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"does/not/exist/*\"]\n",
        )
        .unwrap();
        let suggestions = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        assert!(suggestions.is_empty());
    }

    #[tokio::test]
    async fn missing_manifest_returns_ok_empty() {
        let tmp = TempDir::new().unwrap();
        let suggestions = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        assert!(suggestions.is_empty());
    }

    #[tokio::test]
    async fn ancestor_walk_finds_workspace_above_member_crate() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"a\"]\n",
        )
        .unwrap();
        write_member(tmp.path(), "a", "alpha");
        // ctx.cwd points INTO the member crate — ancestor walk must
        // surface the workspace root above it, not stop at the member.
        let suggestions = CargoWorkspaceMembers::generate_with_root(&tmp.path().join("a"))
            .await
            .unwrap();
        let names: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(names, vec!["alpha"]);
    }

    #[tokio::test]
    async fn description_truncation_parity_with_npm() {
        let tmp = TempDir::new().unwrap();
        let long = "a".repeat(200);
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            format!("[package]\nname = \"big\"\nversion = \"0.1.0\"\ndescription = \"{long}\"\n"),
        )
        .unwrap();
        let suggestions = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        let desc = suggestions[0].description.as_ref().unwrap();
        assert_eq!(desc.chars().count(), MAX_DESCRIPTION_CHARS);
        assert!(desc.ends_with('…'));
    }

    #[tokio::test]
    async fn unsupported_glob_pattern_logged_and_skipped() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"a\", \"crates/**/leaf\"]\n",
        )
        .unwrap();
        write_member(tmp.path(), "a", "alpha");
        // The unsupported `**` pattern is silently skipped; literal `a`
        // still resolves.
        let suggestions = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        let names: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(names, vec!["alpha"]);
    }
}
