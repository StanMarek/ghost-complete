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

use std::collections::{BTreeMap, HashMap};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use anyhow::Result;
use serde::{Deserialize, Deserializer};

use super::MAX_ANCESTOR_WALK;
use crate::providers::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

const MAX_DESCRIPTION_CHARS: usize = 120;

/// Hard cap on cached workspace resolutions. Mirrors `MAX_CACHE_ENTRIES`
/// in `mod.rs`; duplicated rather than shared because `CargoCache` keys
/// off a stamp set rather than (mtime, size) so it can't reuse
/// `MtimeCache` directly.
const MAX_CARGO_CACHE_ENTRIES: usize = 64;

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
    /// Tolerate `version = "x"` (string) AND `version.workspace = true`
    /// (table). The latter is Cargo's workspace-inheritance form and
    /// resolves to `None` here — we don't re-resolve from
    /// `[workspace.package]` (out of scope) but we MUST not let serde
    /// reject the entire member manifest because of it.
    #[serde(default, deserialize_with = "string_or_inherited")]
    version: Option<String>,
    #[serde(default, deserialize_with = "string_or_inherited")]
    description: Option<String>,
}

/// Accept either a TOML string or any non-string shape (table, bool,
/// array). Non-string shapes — including the `{ workspace = true }`
/// inheritance table — collapse to `None` so the parent struct still
/// deserializes. Without this, a single `version.workspace = true`
/// member would silently disappear from `cargo run -p <TAB>` because
/// the whole `toml::from_str::<CargoTomlMin>` call would fail.
fn string_or_inherited<'de, D>(d: D) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d)?;
    Ok(match v {
        toml::Value::String(s) => Some(s),
        _ => None,
    })
}

#[derive(Deserialize, Default)]
struct WorkspaceMin {
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

/// Validated Cargo package name, mirroring `cargo`'s own grammar in
/// `restricted_names::validate_package_name`: first char is an ASCII
/// digit, `_`, or any Unicode `XID_Start`; remaining chars are `-` or
/// any Unicode `XID_Continue`; the bare `_` is reserved. Stricter
/// crates.io-only rejections (Windows reserved filenames, Rust
/// keywords) are intentionally NOT enforced here — `cargo run -p` is
/// happy to invoke any locally-named workspace member that satisfies
/// the manifest grammar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CargoPackageName(String);

impl CargoPackageName {
    pub(crate) fn new(s: String) -> Option<Self> {
        if s.is_empty() || s == "_" {
            return None;
        }
        let mut chars = s.chars();
        let first = chars.next()?;
        if !(first.is_ascii_digit() || first == '_' || unicode_ident::is_xid_start(first)) {
            return None;
        }
        if !chars.all(|c| c == '-' || unicode_ident::is_xid_continue(c)) {
            return None;
        }
        Some(Self(s))
    }
}

impl From<CargoPackageName> for String {
    fn from(v: CargoPackageName) -> Self {
        v.0
    }
}

#[derive(Clone, Debug)]
struct MemberInfo {
    name: CargoPackageName,
    description: Option<String>,
}

/// One probe of a path involved in workspace resolution. Each probe
/// records what was at that path at sample time, including the
/// negative case (`AbsentOrUnreadable`). Files contribute both `mtime`
/// and `size`; directories (glob prefix dirs that drive member
/// expansion) contribute `mtime` only — adding or removing a direct
/// child bumps the parent dir's mtime on every platform we ship on.
///
/// Recording the negative case is what catches the "user added the
/// first crate to a previously-empty workspace" class: the literal
/// member dir / its `Cargo.toml` was AbsentOrUnreadable at first
/// resolve, so transitioning to a real File invalidates the cache
/// instead of serving the cached empty member list.
///
/// `mtime` is `Option<SystemTime>` so a platform that doesn't expose
/// modified-time (some FUSE/NFS mounts; bare `tar -x` of a zero-mtime
/// archive) doesn't get folded into a real `SystemTime::UNIX_EPOCH`
/// value that would compare equal across probes and wedge the cache
/// into a permanent hit. `None == None` is treated as a MISS in
/// `stamp_matches`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum StampState {
    /// Path didn't exist at probe time, OR metadata read failed for
    /// any other reason (PermissionDenied, transient EIO, etc.). The
    /// validity question is binary so all non-success outcomes fold
    /// here; the warn log in `probe_state` distinguishes the cause.
    AbsentOrUnreadable,
    /// Regular file at probe time.
    File {
        mtime: Option<SystemTime>,
        size: u64,
    },
    /// Directory at probe time.
    Dir { mtime: Option<SystemTime> },
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
    ///
    /// `BTreeMap` (rather than `Vec<Stamp>`) deduplicates
    /// double-stamping (e.g. `expand_member_pattern` and
    /// `read_member_info` both stamp the member's `Cargo.toml`) and
    /// gives deterministic test iteration.
    stamps: BTreeMap<PathBuf, StampState>,
}

impl ResolvedRoot {
    fn new() -> Self {
        Self {
            members: Vec::new(),
            stamps: BTreeMap::new(),
        }
    }

    /// Insert a stamp; if the path is already recorded, assert the
    /// state agrees. Disagreement is a programmer error — two
    /// codepaths probed the same path at different times and saw
    /// different things, which would silently corrupt the validity
    /// check. Falls back to overwriting in release builds (warn
    /// instead of panic) so a real-world race doesn't crash the user.
    fn record(&mut self, path: &Path, state: StampState) {
        match self.stamps.get(path) {
            Some(existing) if existing == &state => {}
            Some(existing) => {
                tracing::warn!(
                    path = %path.display(),
                    existing = ?existing,
                    new = ?state,
                    error_id = "cargo.workspace.stamp_conflict",
                    "stamp conflict; overwriting with later observation"
                );
                self.stamps.insert(path.to_path_buf(), state);
            }
            None => {
                self.stamps.insert(path.to_path_buf(), state);
            }
        }
    }
}

struct CargoCacheEntry {
    resolved: ResolvedRoot,
    /// Insertion sequence — used for FIFO-on-insert eviction (mirrors
    /// `MtimeCache`'s `seq` field). Cache hits don't bump this.
    seq: u64,
}

/// Per-process cache for cargo workspace resolution. Distinct from the
/// shared `MtimeCache` because validity here depends on a list of
/// stamps rather than a single (mtime, size) pair on the keyed file.
///
/// Capped at `MAX_CARGO_CACHE_ENTRIES` (64) with FIFO-on-insert
/// eviction, matching `MtimeCache` so a long-lived shell that `cd`s
/// through many distinct cargo projects doesn't grow this cache
/// without bound.
struct CargoCache {
    inner: Mutex<CargoCacheInner>,
}

struct CargoCacheInner {
    entries: HashMap<PathBuf, CargoCacheEntry>,
    next_seq: u64,
}

impl CargoCache {
    fn new() -> Self {
        Self {
            inner: Mutex::new(CargoCacheInner {
                entries: HashMap::new(),
                next_seq: 0,
            }),
        }
    }

    fn get(&self, manifest: &Path) -> Option<ResolvedRoot> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                tracing::error!(
                    error_id = "cargo.workspace.cache_poisoned",
                    "CargoCache mutex poisoned; recovering"
                );
                poisoned.into_inner()
            }
        };
        let entry = guard.entries.get(manifest)?;
        if entry
            .resolved
            .stamps
            .iter()
            .all(|(p, s)| stamp_matches(p, s))
        {
            Some(entry.resolved.clone())
        } else {
            None
        }
    }

    fn store(&self, manifest: PathBuf, resolved: ResolvedRoot) {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                tracing::error!(
                    error_id = "cargo.workspace.cache_poisoned",
                    "CargoCache mutex poisoned; recovering"
                );
                poisoned.into_inner()
            }
        };

        let seq = guard.next_seq;
        guard.next_seq = guard.next_seq.wrapping_add(1);

        if !guard.entries.contains_key(&manifest) && guard.entries.len() >= MAX_CARGO_CACHE_ENTRIES
        {
            if let Some(victim) = guard
                .entries
                .iter()
                .min_by_key(|(_, e)| e.seq)
                .map(|(p, _)| p.clone())
            {
                guard.entries.remove(&victim);
            }
        }

        guard
            .entries
            .insert(manifest, CargoCacheEntry { resolved, seq });
    }

    #[cfg(test)]
    fn clear(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.entries.clear();
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        match self.inner.lock() {
            Ok(g) => g.entries.len(),
            Err(p) => p.into_inner().entries.len(),
        }
    }
}

/// Compare a stored stamp against the current filesystem state. A
/// `None` mtime on either side counts as a MISS — see the
/// `StampState` doc comment for why.
fn stamp_matches(path: &Path, stored: &StampState) -> bool {
    let current = probe_state(path);
    match (stored, &current) {
        (StampState::AbsentOrUnreadable, StampState::AbsentOrUnreadable) => true,
        (
            StampState::File {
                mtime: Some(a),
                size: sa,
            },
            StampState::File {
                mtime: Some(b),
                size: sb,
            },
        ) => a == b && sa == sb,
        (StampState::Dir { mtime: Some(a) }, StampState::Dir { mtime: Some(b) }) => a == b,
        // Any None mtime, or any variant mismatch, is a miss.
        _ => false,
    }
}

fn probe_state(path: &Path) -> StampState {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => StampState::File {
            mtime: meta.modified().ok(),
            size: meta.len(),
        },
        Ok(meta) if meta.is_dir() => StampState::Dir {
            mtime: meta.modified().ok(),
        },
        // Symlink-to-nothing, special file, etc. — treat as absent for
        // validity purposes.
        Ok(_) => StampState::AbsentOrUnreadable,
        Err(e) => {
            if e.kind() != ErrorKind::NotFound {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    error_id = "cargo.workspace.probe_io",
                    "workspace probe IO error; treating as absent"
                );
            }
            StampState::AbsentOrUnreadable
        }
    }
}

/// A `Cargo.toml` file path. The newtype enforces that
/// `find_cargo_root`'s caller can't accidentally pass a directory
/// where a manifest is expected, and centralises the parent-dir
/// derivation so `resolve_workspace` doesn't take two `&Path` args
/// that could be transposed.
#[derive(Clone, Debug)]
pub(crate) struct CargoManifestPath(PathBuf);

impl CargoManifestPath {
    pub(crate) fn new(p: PathBuf) -> Option<Self> {
        if p.is_file() {
            Some(Self(p))
        } else {
            None
        }
    }

    pub(crate) fn dir(&self) -> &Path {
        // Safe: `new` only constructs from a `is_file()` path, which
        // by definition has a parent. Falls back to "." if the
        // platform somehow returns None (root file?).
        self.0.parent().unwrap_or_else(|| Path::new("."))
    }

    pub(crate) fn as_path(&self) -> &Path {
        &self.0
    }
}

static CARGO_CACHE: LazyLock<CargoCache> = LazyLock::new(CargoCache::new);

/// Walk ancestors of `start`. Return the first `Cargo.toml` containing
/// a `[workspace]` table; if none is found, return the nearest
/// `Cargo.toml` so the single-package fallback can still emit the one
/// package name.
pub(crate) fn find_cargo_root(start: &Path) -> Option<CargoManifestPath> {
    let mut nearest: Option<CargoManifestPath> = None;
    let mut current = Some(start);
    for _ in 0..MAX_ANCESTOR_WALK {
        let Some(dir) = current else { break };
        let candidate = dir.join("Cargo.toml");
        if let Some(manifest) = CargoManifestPath::new(candidate) {
            if has_workspace_section(manifest.as_path()) {
                return Some(manifest);
            }
            if nearest.is_none() {
                nearest = Some(manifest);
            }
        }
        current = dir.parent();
    }
    nearest
}

/// Cheap check: does this `Cargo.toml` declare a `[workspace]` table?
///
/// Two-stage:
/// 1. Line-scan for `^\s*\[workspace\b` — a few microseconds vs the
///    dozens-to-hundreds for a full TOML parse on a 50KB+ workspace
///    root manifest. This runs on every keystroke trigger via
///    `find_cargo_root`, so the cost matters.
/// 2. If the cheap check matches, fall back to a real TOML parse to
///    eliminate false positives from commented-out lines or
///    triple-quoted dependency descriptions that contain
///    `[workspace`.
fn has_workspace_section(path: &Path) -> bool {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == ErrorKind::NotFound => return false,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                error_id = "cargo.workspace.probe_read",
                "Cargo.toml unreadable while probing for [workspace]"
            );
            return false;
        }
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                error_id = "cargo.workspace.probe_utf8",
                "Cargo.toml not valid UTF-8 while probing for [workspace]"
            );
            return false;
        }
    };

    if !line_scan_has_workspace(text) {
        return false;
    }

    match toml::from_str::<CargoTomlMin>(text) {
        Ok(CargoTomlMin {
            workspace: Some(_), ..
        }) => true,
        Ok(_) => false,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                error_id = "cargo.workspace.probe_toml",
                "Cargo.toml parse failed while probing for [workspace]"
            );
            false
        }
    }
}

/// Cheap line-scan: is there a line whose first non-whitespace is
/// `[workspace` followed by `]`, `.`, or whitespace? This eliminates
/// the vast majority of `[workspace`-free files without paying the
/// TOML parser cost. False positives (commented-out, triple-quoted
/// strings) are caught by the secondary TOML parse in the caller.
fn line_scan_has_workspace(text: &str) -> bool {
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("[workspace") {
            // Must be `[workspace]`, `[workspace.foo]`, or
            // `[workspace ]` — not `[workspaceextended]`.
            match rest.chars().next() {
                Some(']') | Some('.') | Some(' ') | Some('\t') => return true,
                _ => continue,
            }
        }
    }
    false
}

/// Parse a workspace root and resolve member names, recording every
/// file and directory whose state was sampled along the way. The
/// returned `ResolvedRoot.stamps` is the cache validity contract — a
/// later cache hit is only valid if every recorded stamp still
/// matches.
fn resolve_workspace(manifest: &CargoManifestPath) -> ResolvedRoot {
    let root_dir = manifest.dir();
    let manifest_path = manifest.as_path();

    let mut resolved = ResolvedRoot::new();
    // Always probe — even AbsentOrUnreadable is a stamp the cache
    // needs so that a later `cargo new` (creating the manifest)
    // invalidates correctly.
    resolved.record(manifest_path, probe_state(manifest_path));

    let bytes = match std::fs::read(manifest_path) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                root = %root_dir.display(),
                error = %e,
                error_id = "cargo.workspace.read",
                "Cargo.toml read failed",
            );
            return resolved;
        }
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                root = %root_dir.display(),
                error = %e,
                error_id = "cargo.workspace.utf8",
                "Cargo.toml not valid UTF-8",
            );
            return resolved;
        }
    };
    let parsed: CargoTomlMin = match toml::from_str(text) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                root = %root_dir.display(),
                error = %e,
                error_id = "cargo.workspace.toml_parse",
                "Cargo.toml parse failed",
            );
            return resolved;
        }
    };

    if let Some(ws) = parsed.workspace {
        let exclude_set: std::collections::HashSet<PathBuf> =
            ws.exclude.iter().map(|p| root_dir.join(p)).collect();

        let mut members: Vec<MemberInfo> = Vec::new();
        for pattern in &ws.members {
            for member_path in expand_member_pattern(root_dir, pattern, &mut resolved) {
                if exclude_set.contains(&member_path) {
                    continue;
                }
                if let Some(info) = read_member_info(&member_path, &mut resolved) {
                    members.push(info);
                }
            }
        }
        if members.is_empty() && !ws.members.is_empty() {
            tracing::warn!(
                root = %root_dir.display(),
                patterns = ?ws.members,
                error_id = "cargo.workspace.empty_members",
                "workspace declared members but none resolved to crates with package.name"
            );
        }
        resolved.members = members;
        return resolved;
    }

    if let Some(pkg) = parsed.package {
        if let Some(info) = MemberInfo::from_package(pkg) {
            resolved.members = vec![info];
            return resolved;
        }
    }

    resolved
}

/// Expand one entry of `[workspace].members` into one or more concrete
/// member directory paths (each containing a `Cargo.toml`). The
/// glob-prefix directory's mtime is recorded into `resolved` so that
/// adding or removing a child crate invalidates the cache, even when
/// no `Cargo.toml` content changes.
fn expand_member_pattern(
    root_dir: &Path,
    pattern: &str,
    resolved: &mut ResolvedRoot,
) -> Vec<PathBuf> {
    if let Some(prefix) = pattern.strip_suffix("/*") {
        let prefix_dir = root_dir.join(prefix);
        // Probe the prefix dir even when it doesn't exist — a later
        // `mkdir crates && cargo new crates/foo` should invalidate.
        resolved.record(&prefix_dir, probe_state(&prefix_dir));
        let entries = match std::fs::read_dir(&prefix_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                tracing::warn!(
                    pattern = %pattern,
                    prefix = %prefix_dir.display(),
                    error = %e,
                    error_id = "cargo.workspace.glob_read_dir",
                    "workspace glob: read_dir failed; skipping",
                );
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(
                        prefix = %prefix_dir.display(),
                        error = %e,
                        error_id = "cargo.workspace.glob_entry",
                        "workspace glob: read_dir entry failed; skipping",
                    );
                    continue;
                }
            };
            let p = entry.path();
            if p.is_dir() {
                // Probe the candidate child dir's `Cargo.toml` whether
                // it exists or not — a child dir that exists today
                // without a manifest, then gains one tomorrow, must
                // invalidate the cached "no member found here" result.
                let child_manifest = p.join("Cargo.toml");
                resolved.record(&child_manifest, probe_state(&child_manifest));
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
            error_id = "cargo.workspace.unsupported_glob",
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
    resolved.record(&manifest, probe_state(&manifest));
    if manifest.is_file() {
        vec![dir]
    } else {
        Vec::new()
    }
}

fn read_member_info(member_dir: &Path, resolved: &mut ResolvedRoot) -> Option<MemberInfo> {
    let manifest = member_dir.join("Cargo.toml");
    // Note: `expand_member_pattern` already stamped this manifest
    // (positively, since we only call `read_member_info` for paths it
    // confirmed are files). We re-stamp here to guard against the
    // narrow race where the file vanishes between expansion and read
    // — keeping the stamp set authoritative for what we observed.
    // `record` deduplicates if the state agrees.
    resolved.record(&manifest, probe_state(&manifest));

    let bytes = match std::fs::read(&manifest) {
        Ok(b) => b,
        Err(e) => {
            if e.kind() != ErrorKind::NotFound {
                tracing::warn!(
                    manifest = %manifest.display(),
                    error = %e,
                    error_id = "cargo.workspace.member_io",
                    "workspace member read failed; skipping"
                );
            }
            return None;
        }
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                manifest = %manifest.display(),
                error = %e,
                error_id = "cargo.workspace.member_utf8",
                "workspace member not valid UTF-8; skipping"
            );
            return None;
        }
    };
    let parsed: CargoTomlMin = match toml::from_str(text) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                manifest = %manifest.display(),
                error = %e,
                error_id = "cargo.workspace.member_toml",
                "workspace member parse failed; skipping"
            );
            return None;
        }
    };
    let pkg = parsed.package?;
    let info = MemberInfo::from_package(pkg);
    if info.is_none() {
        tracing::warn!(
            manifest = %manifest.display(),
            error_id = "cargo.workspace.member_no_name",
            "workspace member has no `package.name`; skipping",
        );
    }
    info
}

impl MemberInfo {
    fn from_package(pkg: PackageMin) -> Option<Self> {
        let raw_name = pkg.name?;
        let name = CargoPackageName::new(raw_name)?;
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
        let resolved = match CARGO_CACHE.get(manifest.as_path()) {
            Some(r) => r,
            None => {
                let r = resolve_workspace(&manifest);
                CARGO_CACHE.store(manifest.as_path().to_path_buf(), r.clone());
                r
            }
        };
        Ok(resolved
            .members
            .into_iter()
            .map(|m| Suggestion {
                text: m.name.into(),
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

    /// Regression for pr-test-analyzer-2: literal `members = [...]`
    /// excluded by literal `exclude = [...]` must drop just that
    /// member. Previously only the glob exclude path was tested.
    #[tokio::test]
    async fn literal_exclude_drops_literal_member() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"a\", \"b\"]\nexclude = [\"b\"]\n",
        )
        .unwrap();
        write_member(tmp.path(), "a", "alpha");
        write_member(tmp.path(), "b", "beta");

        let suggestions = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        let names: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(names, vec!["alpha"]);
    }

    /// Regression for pr-test-analyzer-2: excluding a member that
    /// doesn't exist on disk must be a no-op — present members
    /// unaffected, no panic.
    #[tokio::test]
    async fn exclude_of_nonexistent_member_is_noop() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"a\"]\nexclude = [\"ghost\"]\n",
        )
        .unwrap();
        write_member(tmp.path(), "a", "alpha");

        let suggestions = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        let names: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(names, vec!["alpha"]);
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

    /// Regression for pr-test-analyzer-10: multi-byte char truncation
    /// in cargo descriptions mirrors npm's UTF-8 boundary test.
    #[tokio::test]
    async fn truncation_respects_utf8_char_boundaries() {
        let tmp = TempDir::new().unwrap();
        let crab = "\u{1F980}".repeat(200);
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            format!("[package]\nname = \"big\"\ndescription = \"{crab}\"\n"),
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

    /// Regression for code-reviewer-1: workspace inheritance
    /// (`version.workspace = true`, `description.workspace = true`)
    /// must NOT collapse the entire member-manifest deserialization.
    /// Previously the whole `toml::from_str::<CargoTomlMin>` call
    /// failed because `Option<String>` rejected the inheritance
    /// table, dropping the member silently from `cargo run -p <TAB>`.
    #[tokio::test]
    async fn member_with_workspace_inheritance_still_surfaces_name() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"a\"]\n[workspace.package]\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let dir = tmp.path().join("a");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"alpha\"\nversion.workspace = true\ndescription.workspace = true\n",
        )
        .unwrap();

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

    /// Regression for pr-test-analyzer-4: a tracked member crate that
    /// gets DELETED must invalidate the cache so subsequent calls drop
    /// it from the suggestion list.
    #[tokio::test]
    async fn deleted_member_under_glob_invalidates_cache() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        write_member(tmp.path(), "crates/one", "one");
        write_member(tmp.path(), "crates/two", "two");
        let first = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        let mut first_names: Vec<&str> = first.iter().map(|s| s.text.as_str()).collect();
        first_names.sort();
        assert_eq!(first_names, vec!["one", "two"]);

        std::fs::remove_dir_all(tmp.path().join("crates").join("two")).unwrap();
        // Force the prefix dir mtime forward in case the platform
        // debounces same-second updates.
        let prefix = tmp.path().join("crates");
        let future = SystemTime::now() + std::time::Duration::from_secs(120);
        let ft = filetime::FileTime::from_system_time(future);
        filetime::set_file_mtime(&prefix, ft).unwrap();

        let second = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        let names: Vec<&str> = second.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(names, vec!["one"]);
    }

    /// Regression for the second stale-cache class flagged in code
    /// review: the workspace declares `members = ["a"]` but `a/`
    /// doesn't exist yet. First call returns no members. The user then
    /// runs `cargo new a`. Without probing the literal member's
    /// `Cargo.toml` even when it's AbsentOrUnreadable, the cache would
    /// serve the empty list forever — the workspace root manifest
    /// never changed, and there's no member-file stamp to compare
    /// against.
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
    /// invalidate when the prefix transitions from AbsentOrUnreadable
    /// to Dir.
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

    /// Regression for pr-test-analyzer-5: malformed `Cargo.toml` at
    /// the workspace root must (a) not panic, (b) return Ok(empty),
    /// and (c) not poison the cache — fixing the file should surface
    /// real members on the next call.
    #[tokio::test]
    async fn malformed_workspace_root_returns_empty_then_recovers() {
        let tmp = TempDir::new().unwrap();
        // Truncated/invalid TOML.
        std::fs::write(tmp.path().join("Cargo.toml"), "[workspace\nmembers = [\n").unwrap();
        let first = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        assert!(first.is_empty(), "malformed TOML must yield empty Vec");

        // Repair the manifest and add a real member, bumping mtime
        // to invalidate the cached "empty" result.
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"a\"]\n",
        )
        .unwrap();
        write_member(tmp.path(), "a", "alpha");
        let future = SystemTime::now() + std::time::Duration::from_secs(120);
        let ft = filetime::FileTime::from_system_time(future);
        filetime::set_file_mtime(tmp.path().join("Cargo.toml"), ft).unwrap();

        let second = CargoWorkspaceMembers::generate_with_root(tmp.path())
            .await
            .unwrap();
        let names: Vec<&str> = second.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(names, vec!["alpha"]);
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

    #[test]
    fn cargo_package_name_mirrors_cargo_grammar() {
        // Accept what Cargo accepts.
        assert!(CargoPackageName::new("alpha".into()).is_some());
        assert!(CargoPackageName::new("alpha_beta-1".into()).is_some());
        assert!(CargoPackageName::new("_internal".into()).is_some());
        assert!(CargoPackageName::new("7zip".into()).is_some());
        assert!(CargoPackageName::new("1password-cli".into()).is_some());
        assert!(CargoPackageName::new("κ_crate".into()).is_some());
        // Reject what Cargo rejects.
        assert!(CargoPackageName::new(String::new()).is_none());
        assert!(CargoPackageName::new("_".into()).is_none());
        assert!(CargoPackageName::new("-foo".into()).is_none());
        assert!(CargoPackageName::new("foo bar".into()).is_none());
        assert!(CargoPackageName::new("foo:bar".into()).is_none());
        assert!(CargoPackageName::new("foo/bar".into()).is_none());
        assert!(CargoPackageName::new("foo\nbar".into()).is_none());
    }

    #[test]
    fn line_scan_detects_workspace_section() {
        assert!(line_scan_has_workspace("[workspace]\n"));
        assert!(line_scan_has_workspace("  [workspace]\n"));
        assert!(line_scan_has_workspace("[workspace.package]\nfoo = 1\n"));
        assert!(line_scan_has_workspace(
            "[package]\nname=\"a\"\n[workspace]\n"
        ));
        assert!(!line_scan_has_workspace("[workspaceextended]\n"));
        assert!(!line_scan_has_workspace("[package]\nname=\"a\"\n"));
        assert!(!line_scan_has_workspace(""));
    }

    #[test]
    fn cargo_cache_evicts_oldest_at_capacity() {
        let cache = CargoCache::new();
        cache.clear();
        for i in 0..MAX_CARGO_CACHE_ENTRIES {
            cache.store(
                PathBuf::from(format!("/cargo-cache-test/{i}/Cargo.toml")),
                ResolvedRoot::new(),
            );
        }
        assert_eq!(cache.len(), MAX_CARGO_CACHE_ENTRIES);
        cache.store(
            PathBuf::from("/cargo-cache-test/extra/Cargo.toml"),
            ResolvedRoot::new(),
        );
        assert_eq!(
            cache.len(),
            MAX_CARGO_CACHE_ENTRIES,
            "insert past capacity must evict exactly one"
        );
    }
}
