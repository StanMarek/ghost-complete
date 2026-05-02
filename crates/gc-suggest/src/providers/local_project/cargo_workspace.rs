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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use anyhow::Result;
use serde::Deserialize;

use super::MAX_ANCESTOR_WALK;
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

#[derive(Clone, Debug)]
struct MemberInfo {
    name: String,
    description: Option<String>,
}

/// One probe of a path involved in workspace resolution. Each probe
/// records what was at that path at sample time, including the
/// negative case (`Missing`). Files contribute both `mtime` and
/// `size`; directories (glob prefix dirs that drive member expansion)
/// contribute `mtime` only — adding or removing a direct child bumps
/// the parent dir's mtime on every platform we ship on.
///
/// Recording the negative case is what catches the "user added the
/// first crate to a previously-empty workspace" class: the literal
/// member dir / its `Cargo.toml` was Missing at first resolve, so
/// transitioning to a real File invalidates the cache instead of
/// serving the cached empty member list.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    path: PathBuf,
    state: StampState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum StampState {
    /// Path did not exist when probed.
    Missing,
    /// Regular file at probe time.
    File { mtime: SystemTime, size: u64 },
    /// Directory at probe time.
    Dir { mtime: SystemTime },
}

#[derive(Clone)]
struct ResolvedRoot {
    members: Vec<MemberInfo>,
    /// Every file + directory whose state was sampled to produce
    /// `members`. Cache hits require every stamp to still match the
    /// live filesystem state, which catches member-file edits and new
    /// crates appearing under a glob-expanded prefix — neither of
    /// which mutate the workspace root `Cargo.toml` itself, so
    /// keying off the root alone would silently return stale data.
    stamps: Vec<Stamp>,
}

/// Per-process cache for cargo workspace resolution. Distinct from the
/// shared `MtimeCache` because validity here depends on a list of
/// stamps rather than a single (mtime, size) pair on the keyed file.
struct CargoCache {
    inner: Mutex<HashMap<PathBuf, ResolvedRoot>>,
}

impl CargoCache {
    fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    fn get(&self, manifest: &Path) -> Option<ResolvedRoot> {
        let guard = self.inner.lock().ok()?;
        let entry = guard.get(manifest)?.clone();
        if entry.stamps.iter().all(stamp_still_matches) {
            Some(entry)
        } else {
            None
        }
    }

    fn store(&self, manifest: PathBuf, resolved: ResolvedRoot) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.insert(manifest, resolved);
        }
    }
}

fn stamp_still_matches(stamp: &Stamp) -> bool {
    probe_state(&stamp.path) == stamp.state
}

fn probe_state(path: &Path) -> StampState {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => StampState::File {
            mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            size: meta.len(),
        },
        Ok(meta) if meta.is_dir() => StampState::Dir {
            mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        },
        // Anything else (symlink-to-nothing, special file, IO error)
        // is folded into Missing — the validity question is binary
        // ("does the workspace look the same?") and recording odd file
        // types as their own variant would proliferate without
        // improving the answer.
        _ => StampState::Missing,
    }
}

/// Record the current state of `path` (including the negative
/// `Missing` case) so the cache can detect both "this file changed"
/// AND "this file didn't exist before but does now" on the next call.
fn stamp_path(path: &Path) -> Stamp {
    Stamp {
        path: path.to_path_buf(),
        state: probe_state(path),
    }
}

static CARGO_CACHE: LazyLock<CargoCache> = LazyLock::new(CargoCache::new);

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
        Ok(CargoTomlMin {
            workspace: Some(_),
            ..
        })
    )
}

/// Parse a workspace root and resolve member names, recording every
/// file and directory whose state was sampled along the way. The
/// returned `ResolvedRoot.stamps` is the cache validity contract — a
/// later cache hit is only valid if every recorded stamp still
/// matches.
fn resolve_workspace(manifest: &Path, root_dir: &Path) -> ResolvedRoot {
    let mut stamps: Vec<Stamp> = Vec::new();
    // Always probe — even Missing is a stamp the cache needs so that a
    // later `cargo new` (creating the manifest) invalidates correctly.
    stamps.push(stamp_path(manifest));

    let bytes = match std::fs::read(manifest) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                root = %root_dir.display(),
                error = %e,
                "Cargo.toml read failed",
            );
            return ResolvedRoot {
                members: Vec::new(),
                stamps,
            };
        }
    };
    let parsed: CargoTomlMin = match std::str::from_utf8(&bytes)
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
                stamps,
            };
        }
    };

    if let Some(ws) = parsed.workspace {
        let exclude_set: std::collections::HashSet<PathBuf> =
            ws.exclude.iter().map(|p| root_dir.join(p)).collect();

        let mut members: Vec<MemberInfo> = Vec::new();
        for pattern in &ws.members {
            for member_path in expand_member_pattern(root_dir, pattern, &mut stamps) {
                if exclude_set.contains(&member_path) {
                    continue;
                }
                if let Some(info) = read_member_info(&member_path, &mut stamps) {
                    members.push(info);
                }
            }
        }
        return ResolvedRoot { members, stamps };
    }

    if let Some(pkg) = parsed.package {
        if let Some(info) = MemberInfo::from_package(pkg) {
            return ResolvedRoot {
                members: vec![info],
                stamps,
            };
        }
    }

    ResolvedRoot {
        members: Vec::new(),
        stamps,
    }
}

/// Expand one entry of `[workspace].members` into one or more concrete
/// member directory paths (each containing a `Cargo.toml`). The
/// glob-prefix directory's mtime is recorded into `stamps` so that
/// adding or removing a child crate invalidates the cache, even when
/// no `Cargo.toml` content changes.
fn expand_member_pattern(root_dir: &Path, pattern: &str, stamps: &mut Vec<Stamp>) -> Vec<PathBuf> {
    if let Some(prefix) = pattern.strip_suffix("/*") {
        let prefix_dir = root_dir.join(prefix);
        // Probe the prefix dir even when it doesn't exist — a later
        // `mkdir crates && cargo new crates/foo` should invalidate.
        stamps.push(stamp_path(&prefix_dir));
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
            if p.is_dir() {
                // Probe the candidate child dir's `Cargo.toml` whether
                // it exists or not — a child dir that exists today
                // without a manifest, then gains one tomorrow, must
                // invalidate the cached "no member found here" result.
                let child_manifest = p.join("Cargo.toml");
                stamps.push(stamp_path(&child_manifest));
                if child_manifest.is_file() {
                    out.push(p);
                }
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
    let manifest = dir.join("Cargo.toml");
    // Probe the literal member's `Cargo.toml` whether it exists or
    // not, so the "user adds the first crate to a previously-empty
    // workspace" path invalidates on the manifest's appearance rather
    // than serving the cached empty member list forever.
    stamps.push(stamp_path(&manifest));
    if manifest.is_file() {
        vec![dir]
    } else {
        Vec::new()
    }
}

fn read_member_info(member_dir: &Path, stamps: &mut Vec<Stamp>) -> Option<MemberInfo> {
    let manifest = member_dir.join("Cargo.toml");
    // Note: `expand_member_pattern` already stamped this manifest
    // (positively, since we only call `read_member_info` for paths it
    // confirmed are files). We re-stamp here to guard against the
    // narrow race where the file vanishes between expansion and read
    // — keeping the stamp set authoritative for what we observed.
    stamps.push(stamp_path(&manifest));
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
        let resolved = match CARGO_CACHE.get(&manifest) {
            Some(r) => r,
            None => {
                let r = resolve_workspace(&manifest, &manifest_dir);
                CARGO_CACHE.store(manifest.clone(), r.clone());
                r
            }
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
        assert_eq!(
            suggestions[0].description.as_deref(),
            Some("0.2.0 — a crate")
        );
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

    /// Regression for the stale-cache class flagged in code review:
    /// editing a member's `Cargo.toml` (e.g. renaming the package) must
    /// invalidate the cache even though the workspace root manifest
    /// itself is untouched. The cache must consult every member-file
    /// stamp, not just the root.
    #[tokio::test]
    async fn member_cargo_toml_edit_invalidates_cached_members() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"a\"]\n",
        )
        .unwrap();
        write_member(tmp.path(), "a", "old_name");
        let first = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        assert_eq!(first[0].text, "old_name");

        // Rewrite the member manifest in-place AND bump its mtime
        // forward (touch-without-edit pattern). Stale cache would still
        // serve "old_name"; correct cache must re-read and surface the
        // new package name.
        let member_manifest = tmp.path().join("a").join("Cargo.toml");
        std::fs::write(
            &member_manifest,
            "[package]\nname = \"new_name\"\nversion = \"0.2.0\"\n",
        )
        .unwrap();
        let future = SystemTime::now() + std::time::Duration::from_secs(120);
        let ft = filetime::FileTime::from_system_time(future);
        filetime::set_file_mtime(&member_manifest, ft).unwrap();

        let second = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        assert_eq!(second[0].text, "new_name");
    }

    /// Regression: a new crate added under a `members = ["crates/*"]`
    /// glob doesn't change the workspace root `Cargo.toml`. Without
    /// validating the prefix directory's mtime, the cache would
    /// silently omit the new crate from `cargo run -p <TAB>`.
    #[tokio::test]
    async fn new_crate_under_glob_prefix_invalidates_cache() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        write_member(tmp.path(), "crates/one", "one");
        let first = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        let first_names: Vec<&str> = first.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(first_names, vec!["one"]);

        // Add a sibling crate under the glob prefix. The workspace
        // root's mtime is unchanged, but the prefix directory's mtime
        // must change as a result of the new entry — that's the signal
        // we hang invalidation on.
        write_member(tmp.path(), "crates/two", "two");
        // Belt-and-suspenders: explicitly bump the prefix dir mtime in
        // case the platform doesn't (some filesystems debounce dir
        // mtime updates within the same wall-clock second).
        let prefix = tmp.path().join("crates");
        let future = SystemTime::now() + std::time::Duration::from_secs(120);
        let ft = filetime::FileTime::from_system_time(future);
        filetime::set_file_mtime(&prefix, ft).unwrap();

        let second = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        let mut second_names: Vec<&str> = second.iter().map(|s| s.text.as_str()).collect();
        second_names.sort();
        assert_eq!(second_names, vec!["one", "two"]);
    }

    /// Regression for the second stale-cache class flagged in code
    /// review: the workspace declares `members = ["a"]` but `a/`
    /// doesn't exist yet. First call returns no members. The user then
    /// runs `cargo new a`. Without probing the literal member's
    /// `Cargo.toml` even when it's Missing, the cache would serve the
    /// empty list forever — the workspace root manifest never changed,
    /// and there's no member-file stamp to compare against.
    #[tokio::test]
    async fn literal_member_appearing_invalidates_empty_cache() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"a\"]\n",
        )
        .unwrap();
        // First call: no `a/Cargo.toml` exists.
        let first = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        assert!(first.is_empty(), "expected no members before crate created");

        // User creates the crate.
        write_member(tmp.path(), "a", "alpha");

        let second = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        let names: Vec<&str> = second.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(names, vec!["alpha"]);
    }

    /// Regression for a sibling case under glob expansion: a child
    /// directory of `crates/` exists but has no `Cargo.toml` yet
    /// (perhaps half-set-up). On first resolve the directory has no
    /// member to enumerate; on second resolve the user has dropped a
    /// `Cargo.toml` into the child dir. Must invalidate.
    #[tokio::test]
    async fn cargo_toml_added_to_existing_glob_child_invalidates() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        let half_setup = tmp.path().join("crates").join("foo");
        std::fs::create_dir_all(&half_setup).unwrap();

        let first = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        assert!(
            first.is_empty(),
            "child dir without Cargo.toml is not a member"
        );

        // User finishes setting up the crate.
        std::fs::write(
            half_setup.join("Cargo.toml"),
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let second = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        let names: Vec<&str> = second.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(names, vec!["foo"]);
    }

    /// Regression for the glob-prefix-doesn't-exist-yet case. Workspace
    /// declares `members = ["crates/*"]` but `crates/` itself doesn't
    /// exist at first resolve time. First call produces nothing; user
    /// then `mkdir crates && cargo new crates/foo`. The cache must
    /// invalidate when the prefix transitions from Missing to Dir.
    #[tokio::test]
    async fn glob_prefix_dir_appearing_invalidates_empty_cache() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        let first = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        assert!(first.is_empty());

        write_member(tmp.path(), "crates/foo", "foo");

        let second = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        let names: Vec<&str> = second.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(names, vec!["foo"]);
    }

    /// Sanity check: when nothing on disk has changed, the second call
    /// returns the same data without re-doing the work. We can't
    /// observe "no work" directly without instrumenting the cache, but
    /// we can at least confirm the result is consistent across two
    /// back-to-back calls.
    #[tokio::test]
    async fn cache_hit_returns_same_data_on_repeated_calls() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"a\", \"b\"]\n",
        )
        .unwrap();
        write_member(tmp.path(), "a", "alpha");
        write_member(tmp.path(), "b", "beta");

        let first = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        let second = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        let first_names: Vec<&str> = first.iter().map(|s| s.text.as_str()).collect();
        let second_names: Vec<&str> = second.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(first_names, second_names);
    }
}
