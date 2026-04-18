//! Frecency-weighted scoring for suggestions.
//!
//! Frequently **and** recently used completions rank higher. Uses exponential
//! decay with a half-life of 72 hours (3 days) — the full usage history is
//! compressed into a single f64 per entry.
//!
//! Keys are scoped by command **and** suggestion kind: an argument completion
//! under `git` is stored as `git\0sub\0status`, distinct from `docker\0sub\0status`.
//! Different kinds under the same command are also distinct: `git\0branch\0main`
//! vs `git\0file\0main`. History items are always keyed without a command scope
//! (e.g. `hist\0git push`) because the text IS the full command.
//!
//! Storage lives at `$XDG_STATE_HOME/ghost-complete/frecency.json` (falling
//! back to `~/.local/state/ghost-complete/frecency.json`). On first run the
//! legacy `~/.config/ghost-complete/frecency.json` is read and migrated to
//! the new location on the next save.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::types::{Suggestion, SuggestionKind};

/// File name within the state/config directory.
const FRECENCY_FILE: &str = "frecency.json";

/// Recency half-life in hours (3 days).
const HALF_LIFE_HOURS: f64 = 72.0;

/// Maximum entries to persist. Lowest-scoring entries are evicted on save.
const MAX_ENTRIES: usize = 1000;

/// Batch-save threshold — saves to disk every N record() calls.
/// Low enough to persist quickly during normal use, high enough to
/// avoid disk I/O on every single acceptance.
const SAVE_EVERY: u32 = 3;

/// Separator between command and text in frecency keys.
/// NUL byte is safe because it can never appear in shell arguments.
const KEY_SEP: char = '\0';

/// On-disk schema version. Bump whenever the serialized layout changes in a
/// non-backward-compatible way; readers refuse to load a newer version and
/// treat the data as empty so the running binary can't corrupt a file that
/// a future release wrote. The original file stays on disk untouched (we
/// also refuse to overwrite it on save) so a downgrade isn't destructive.
const CURRENT_VERSION: u32 = 1;

/// Upper bound on a single entry's `stored_score`. Protects against runaway
/// drift if the clock ever jumps backwards — decay would then be negative
/// and repeated `+1.0` bumps could climb toward `f64::INFINITY`. Capping at
/// 1e18 leaves ample headroom over any plausible real-world usage while
/// keeping the value finite and well below `f64::MAX`.
const MAX_STORED_SCORE: f64 = 1e18;

fn current_version() -> u32 {
    CURRENT_VERSION
}

/// A single entry using exponential decay with single-number compression.
/// The stored_score encodes the entire usage history: on each visit, the
/// existing score is decayed to the current time and 1.0 is added.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrecencyEntry {
    pub stored_score: f64,
    /// Seconds since the Unix epoch — the reference time for decay computation.
    pub reference_secs: u64,
}

impl FrecencyEntry {
    /// Compute the actual (decayed) score at the current time.
    fn actual_score(&self, now_secs: u64) -> f64 {
        let elapsed_hours = (now_secs.saturating_sub(self.reference_secs)) as f64 / 3600.0;
        self.stored_score / 2.0_f64.powf(elapsed_hours / HALF_LIFE_HOURS)
    }
}

/// Legacy format from pre-v0.5.0 releases.
#[derive(Deserialize)]
struct LegacyEntry {
    frequency: u32,
    last_used_secs: u64,
}

/// Versioned on-disk envelope. Writers always emit this form; readers accept
/// either this form or the pre-versioning bare map for backward compatibility
/// (and the even older `LegacyEntry` format for pre-v0.5.0 data).
#[derive(Serialize, Deserialize)]
struct VersionedStore {
    #[serde(default = "current_version")]
    version: u32,
    entries: HashMap<String, FrecencyEntry>,
}

struct FrecencyInner {
    entries: HashMap<String, FrecencyEntry>,
    dirty_count: u32,
}

/// In-memory frecency database backed by a JSON file on disk.
/// Uses interior mutability so all methods take `&self`.
pub struct FrecencyDb {
    inner: Mutex<FrecencyInner>,
    path: Option<PathBuf>,
}

// Manual Debug impl since Mutex doesn't derive Debug nicely
impl std::fmt::Debug for FrecencyDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrecencyDb")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Resolve the preferred on-disk path for the frecency store:
/// `$XDG_STATE_HOME/ghost-complete/frecency.json` if `XDG_STATE_HOME` is
/// set to a non-empty value, else `~/.local/state/ghost-complete/frecency.json`.
/// Returns `None` when the home directory can't be determined *and* there is
/// no `XDG_STATE_HOME` — callers degrade to an in-memory-only store.
fn resolve_state_path() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            return Some(
                PathBuf::from(xdg)
                    .join("ghost-complete")
                    .join(FRECENCY_FILE),
            );
        }
    }
    dirs::home_dir().map(|h| {
        h.join(".local")
            .join("state")
            .join("ghost-complete")
            .join(FRECENCY_FILE)
    })
}

/// Legacy `~/.config/ghost-complete/frecency.json` path — used only for the
/// one-time read-only migration when the new state path has no file yet.
fn resolve_legacy_path() -> Option<PathBuf> {
    gc_config::config_dir().map(|d| d.join(FRECENCY_FILE))
}

/// Load entries for `path`. If `path` doesn't exist but the legacy config
/// path does, read from the legacy path (the next save writes to `path`,
/// completing the migration). Emits an `info!` once per load so the
/// migration is traceable.
fn load_entries_with_migration(path: &PathBuf) -> HashMap<String, FrecencyEntry> {
    if path.exists() {
        return match std::fs::read_to_string(path) {
            Ok(s) => FrecencyDb::deserialize_entries(&s),
            Err(e) => {
                tracing::warn!("frecency file unreadable: {e}");
                HashMap::new()
            }
        };
    }
    if let Some(legacy) = resolve_legacy_path() {
        if legacy.exists() && legacy != *path {
            match std::fs::read_to_string(&legacy) {
                Ok(s) => {
                    let entries = FrecencyDb::deserialize_entries(&s);
                    tracing::info!(
                        "migrating {} frecency entries from {} to {}",
                        entries.len(),
                        legacy.display(),
                        path.display()
                    );
                    return entries;
                }
                Err(e) => {
                    tracing::warn!("legacy frecency file unreadable: {e}");
                }
            }
        }
    }
    HashMap::new()
}

/// Build a frecency key scoped by command and suggestion kind.
///
/// Keys use the format `command\0kind_tag\0text` for argument-position
/// completions, or `kind_tag\0text` for command-position completions.
/// This prevents both cross-command bleed (`git\0sub\0status` vs
/// `docker\0sub\0status`) and same-command kind collisions
/// (`git\0branch\0main` vs `git\0file\0main`).
pub fn frecency_key(command: Option<&str>, kind: SuggestionKind, text: &str) -> String {
    let tag = kind.key_tag();
    match command {
        Some(cmd) if !cmd.is_empty() => format!("{cmd}{KEY_SEP}{tag}{KEY_SEP}{text}"),
        _ => format!("{tag}{KEY_SEP}{text}"),
    }
}

impl FrecencyDb {
    /// Acquire the inner mutex, recovering from poisoning instead of panicking.
    /// A best-effort subsystem should never crash the proxy.
    fn lock_inner(&self) -> MutexGuard<'_, FrecencyInner> {
        self.inner.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("frecency mutex poisoned — recovering");
            poisoned.into_inner()
        })
    }

    /// Load from the default state directory, honouring `$XDG_STATE_HOME`
    /// (frecency is state, not config). Performs a one-time read-only
    /// migration from the legacy `~/.config/ghost-complete/frecency.json`
    /// location when the new path doesn't yet exist. Returns an empty
    /// database on any I/O or parse error so callers never have to handle
    /// failures.
    pub fn load() -> Self {
        let path = resolve_state_path();
        let entries = match &path {
            Some(p) => load_entries_with_migration(p),
            None => HashMap::new(),
        };
        Self {
            inner: Mutex::new(FrecencyInner {
                entries,
                dirty_count: 0,
            }),
            path,
        }
    }

    /// Load from a specific path (useful for tests).
    #[cfg(test)]
    pub fn load_from(path: PathBuf) -> Self {
        let entries = std::fs::read_to_string(&path)
            .ok()
            .map(|s| Self::deserialize_entries(&s))
            .unwrap_or_default();
        Self {
            inner: Mutex::new(FrecencyInner {
                entries,
                dirty_count: 0,
            }),
            path: Some(path),
        }
    }

    /// Create an empty database that never touches disk.
    pub fn empty() -> Self {
        Self {
            inner: Mutex::new(FrecencyInner {
                entries: HashMap::new(),
                dirty_count: 0,
            }),
            path: None,
        }
    }

    /// Deserialize entries, accepting three on-disk shapes:
    ///
    /// 1. The current versioned envelope `{"version": N, "entries": {..}}`.
    ///    If `version > CURRENT_VERSION` we log a warning and return empty
    ///    so the running binary never corrupts a newer-format file. The
    ///    file stays on disk untouched until a save path writes over it —
    ///    and `save_snapshot` also refuses to overwrite a future-version
    ///    file, so a downgrade is non-destructive.
    /// 2. The pre-versioning bare map `{key: FrecencyEntry, ..}` (v0.5.0
    ///    through the version-introduction release).
    /// 3. The legacy `{frequency, last_used_secs}` format (pre-v0.5.0).
    fn deserialize_entries(json: &str) -> HashMap<String, FrecencyEntry> {
        // Try the versioned envelope first (requires both fields present).
        if let Ok(store) = serde_json::from_str::<VersionedStore>(json) {
            if store.version > CURRENT_VERSION {
                tracing::warn!(
                    "frecency file schema version {} is newer than supported ({CURRENT_VERSION}) — treating as empty; preserving file on disk",
                    store.version
                );
                return HashMap::new();
            }
            return store.entries;
        }

        // Pre-versioning bare map: `{key: FrecencyEntry, ..}`.
        if let Ok(map) = serde_json::from_str::<HashMap<String, FrecencyEntry>>(json) {
            return map;
        }

        // Legacy pre-v0.5.0 format: `{frequency, last_used_secs}`.
        match serde_json::from_str::<HashMap<String, LegacyEntry>>(json) {
            Ok(legacy) => {
                tracing::info!(
                    "migrating {} frecency entries from legacy format",
                    legacy.len()
                );
                legacy
                    .into_iter()
                    .map(|(k, v)| {
                        (
                            k,
                            FrecencyEntry {
                                stored_score: v.frequency as f64,
                                reference_secs: v.last_used_secs,
                            },
                        )
                    })
                    .collect()
            }
            Err(e) => {
                tracing::warn!("frecency data corrupt, starting fresh: {e}");
                HashMap::new()
            }
        }
    }

    /// Peek at the envelope version on disk without loading entries.
    ///
    /// Returns `Ok(None)` when the file is absent (benign first-run case).
    /// Returns `Ok(Some(v))` for a readable versioned envelope, or
    /// `Ok(Some(0))` for any parse that doesn't match `VersionOnly` — a
    /// pre-versioning bare map is treated as version 0.
    ///
    /// Returns `Err` for any other I/O failure (permission denied, disk
    /// fault). Collapsing those into "missing" would defeat the downgrade
    /// guard the caller installs — it would then happily overwrite a file
    /// it simply can't read right now.
    ///
    /// Deliberately parses ONLY the `version` field. If a future release
    /// changes the shape of `entries`, deserializing the whole
    /// `VersionedStore` here would fail — which is exactly the scenario
    /// the guard exists to prevent.
    fn peek_disk_version(path: &std::path::Path) -> std::io::Result<Option<u32>> {
        #[derive(Deserialize)]
        struct VersionOnly {
            version: u32,
        }
        match std::fs::read_to_string(path) {
            Ok(json) => Ok(Some(
                serde_json::from_str::<VersionOnly>(&json)
                    .map(|s| s.version)
                    .unwrap_or(0),
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => {
                tracing::warn!(
                    "frecency peek failed for {}: {} ({:?})",
                    path.display(),
                    e,
                    e.kind()
                );
                Err(e)
            }
        }
    }

    /// Persist a snapshot of entries to disk, merging with any peer that
    /// wrote to the same file while we were accumulating in memory. Prunes
    /// to `MAX_ENTRIES` by evicting entries with the lowest actual scores.
    /// Uses atomic write (tmp + rename).
    ///
    /// For each key present in *either* the on-disk file or the in-memory
    /// snapshot, we pick the entry with the larger decayed score at
    /// `now_secs()`. Both sides are normalized to the same time origin
    /// before comparison so that a peer whose reference time is days old
    /// doesn't clobber a fresh bump just because its raw `stored_score`
    /// happens to be larger.
    ///
    /// Takes the snapshot by value — the caller is responsible for cloning
    /// the entries out from under the mutex *before* invoking this. Disk I/O
    /// happens with no lock held, so concurrent `score()` / `boost_scores()`
    /// callers are not blocked by a slow filesystem.
    ///
    /// Returns `true` on success, `false` on any failure — including the
    /// case where the existing file is unreadable (refuse to clobber data
    /// whose contents we can't verify).
    fn save_snapshot(snapshot: HashMap<String, FrecencyEntry>, path: &Option<PathBuf>) -> bool {
        let Some(ref path) = path else { return true };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!("frecency dir creation failed: {e}");
                return false;
            }
        }

        // Forward-compat guard: if a newer binary wrote a higher schema
        // version to this file, don't overwrite it from a downgraded run.
        // Any I/O error here (permission denied, disk fault, etc.) means
        // we can't verify the file is safe to overwrite — refuse.
        match Self::peek_disk_version(path) {
            Ok(Some(v)) if v > CURRENT_VERSION => {
                tracing::warn!(
                    "frecency file at {} has newer schema version {v}; refusing to overwrite from running CURRENT_VERSION={CURRENT_VERSION}",
                    path.display()
                );
                return false;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    "frecency file at {} unreadable ({:?}); refusing to overwrite: {e}",
                    path.display(),
                    e.kind()
                );
                return false;
            }
        }

        let now = now_secs();

        // Merge-on-save: re-read whatever a peer process may have written
        // while we held our in-memory state, and pick the winner per key.
        let merged = match std::fs::read_to_string(path) {
            Ok(s) => Self::merge_snapshots(Self::deserialize_entries(&s), snapshot, now),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => snapshot,
            Err(e) => {
                tracing::warn!(
                    "frecency read-for-merge failed at {} ({:?}); refusing to overwrite: {e}",
                    path.display(),
                    e.kind()
                );
                return false;
            }
        };

        // Prune if over the cap — keep the highest-scoring entries.
        let entries_map = if merged.len() > MAX_ENTRIES {
            let mut scored: Vec<_> = merged
                .into_iter()
                .map(|(k, v)| {
                    let score = v.actual_score(now);
                    (k, v, score)
                })
                .collect();
            scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(MAX_ENTRIES);
            scored
                .into_iter()
                .map(|(k, v, _)| (k, v))
                .collect::<HashMap<_, _>>()
        } else {
            merged
        };

        let envelope = VersionedStore {
            version: CURRENT_VERSION,
            entries: entries_map,
        };

        let json = match serde_json::to_string_pretty(&envelope) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("frecency serialize error: {e}");
                return false;
            }
        };

        // Each writer gets a unique temp file in the same directory as the
        // target (so `persist()` is an atomic same-FS rename). A shared temp
        // name (e.g. `frecency.json.tmp`) would let two concurrent
        // ghost-complete processes clobber each other's half-written file
        // before rename — the loser then restores dirty in-memory state that
        // is lost on exit, silently dropping accepted completions.
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let mut tmp = match tempfile::NamedTempFile::new_in(parent) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("frecency save error (create tmp): {e}");
                return false;
            }
        };
        if let Err(e) = std::io::Write::write_all(tmp.as_file_mut(), json.as_bytes()) {
            tracing::warn!("frecency save error (write tmp): {e}");
            return false;
        }
        if let Err(e) = tmp.persist(path) {
            tracing::warn!("frecency save error (persist): {e}");
            return false;
        }

        true
    }

    /// Merge two snapshots, keeping the entry with the larger decayed score
    /// per key. Both sides are normalized to the same `now_secs` so the
    /// comparison is apples-to-apples even when the two processes last
    /// touched their entries at wildly different times.
    fn merge_snapshots(
        mut disk: HashMap<String, FrecencyEntry>,
        mem: HashMap<String, FrecencyEntry>,
        now_secs: u64,
    ) -> HashMap<String, FrecencyEntry> {
        for (key, mem_entry) in mem {
            match disk.get(&key) {
                Some(disk_entry) => {
                    let disk_score = disk_entry.actual_score(now_secs);
                    let mem_score = mem_entry.actual_score(now_secs);
                    if mem_score >= disk_score {
                        disk.insert(key, mem_entry);
                    }
                    // else: keep disk_entry — peer wrote a fresher/larger score
                }
                None => {
                    disk.insert(key, mem_entry);
                }
            }
        }
        disk
    }

    /// Record a completion acceptance — decays existing score and adds 1.0.
    /// Batches disk writes: flushes every `SAVE_EVERY` records.
    pub fn record(&self, key: &str) {
        // Update the in-memory state under the lock, then either return
        // empty-handed or hand back a cloned snapshot for the slow disk
        // write below. The mutex is released as soon as this block exits.
        let snapshot = {
            let mut inner = self.lock_inner();
            let now = now_secs();

            let entry = inner
                .entries
                .entry(key.to_string())
                .or_insert(FrecencyEntry {
                    stored_score: 0.0,
                    reference_secs: now,
                });

            // Decay existing score to current time, then add 1.0, clamping
            // to MAX_STORED_SCORE to prevent any infinity drift.
            let actual = entry.actual_score(now);
            entry.stored_score = (actual + 1.0).min(MAX_STORED_SCORE);
            entry.reference_secs = now;

            inner.dirty_count += 1;
            if inner.dirty_count >= SAVE_EVERY {
                // Eagerly reset under the lock so a concurrent record()
                // doesn't also try to save the same data. If the disk
                // write fails we restore the dirty state below.
                inner.dirty_count = 0;
                Some(inner.entries.clone())
            } else {
                None
            }
            // Lock dropped here.
        };

        if let Some(snapshot) = snapshot {
            if !Self::save_snapshot(snapshot, &self.path) {
                // Restore dirty state so the next record() retries the save.
                let mut inner = self.lock_inner();
                inner.dirty_count = inner.dirty_count.saturating_add(SAVE_EVERY);
            }
        }
    }

    /// Flush any unsaved records to disk. Call on proxy shutdown.
    pub fn flush(&self) {
        let snapshot = {
            let mut inner = self.lock_inner();
            if inner.dirty_count == 0 {
                None
            } else {
                inner.dirty_count = 0;
                Some(inner.entries.clone())
            }
            // Lock dropped here.
        };

        if let Some(snapshot) = snapshot {
            if !Self::save_snapshot(snapshot, &self.path) {
                // Restore dirty state so a future flush()/record() retries.
                let mut inner = self.lock_inner();
                inner.dirty_count = inner.dirty_count.saturating_add(1);
            }
        }
    }

    /// Compute the frecency score for a completion key.
    /// Returns `0.0` for unknown entries.
    pub fn score(&self, key: &str) -> f64 {
        let inner = self.lock_inner();
        inner
            .entries
            .get(key)
            .map(|e| e.actual_score(now_secs()))
            .unwrap_or(0.0)
    }

    /// Apply frecency bonuses to a batch of suggestions. Acquires the lock once
    /// and reads the clock once, avoiding per-suggestion overhead.
    ///
    /// `command` is the current command name (e.g. "git"), or `None` for
    /// command-position completions.
    pub fn boost_scores(&self, suggestions: &mut [Suggestion], command: Option<&str>) {
        let inner = self.lock_inner();
        let now = now_secs();
        for suggestion in suggestions.iter_mut() {
            // History items are full commands — always keyed without command scope
            let cmd = if suggestion.kind == SuggestionKind::History {
                None
            } else {
                command
            };
            let key = frecency_key(cmd, suggestion.kind, &suggestion.text);
            if let Some(entry) = inner.entries.get(&key) {
                let frecency = entry.actual_score(now);
                if frecency > 0.0 {
                    // Scale frecency into a bonus that meaningfully affects
                    // nucleo's u32 score range. The effective bonus depends on
                    // both recency and accumulated uses (decayed).
                    let bonus = (frecency * 100.0).min(u32::MAX as f64) as u32;
                    suggestion.score = suggestion.score.saturating_add(bonus);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{SuggestionKind, SuggestionSource};

    #[test]
    fn empty_db_returns_zero_score() {
        let db = FrecencyDb::empty();
        assert_eq!(db.score("anything"), 0.0);
    }

    #[test]
    fn record_increments_score() {
        let db = FrecencyDb::empty();
        db.record("git push");
        let s1 = db.score("git push");
        assert!(
            s1 > 0.9 && s1 <= 1.0,
            "first record should score ~1.0, got {s1}"
        );

        db.record("git push");
        let s2 = db.score("git push");
        assert!(
            s2 > 1.9 && s2 <= 2.0,
            "second record should score ~2.0, got {s2}"
        );
    }

    #[test]
    fn score_decays_over_time() {
        let db = FrecencyDb::empty();
        {
            let mut inner = db.lock_inner();
            // Simulate a command used 10 times, 3 days ago (one half-life)
            let three_days_ago = now_secs() - (72 * 3600);
            inner.entries.insert(
                "old command".into(),
                FrecencyEntry {
                    stored_score: 10.0,
                    reference_secs: three_days_ago,
                },
            );
        }

        let s = db.score("old command");
        // After one half-life, score should be ~5.0
        assert!(
            (s - 5.0).abs() < 0.2,
            "expected score near 5.0 after one half-life, got {s}"
        );
    }

    #[test]
    fn score_recent_command() {
        let db = FrecencyDb::empty();
        {
            let mut inner = db.lock_inner();
            inner.entries.insert(
                "cargo build".into(),
                FrecencyEntry {
                    stored_score: 10.0,
                    reference_secs: now_secs(),
                },
            );
        }

        let s = db.score("cargo build");
        assert!(s > 9.5, "expected score near 10.0, got {s}");
        assert!(s <= 10.0, "expected score <= 10.0, got {s}");
    }

    #[test]
    fn boost_scores_adds_bonus() {
        let db = FrecencyDb::empty();
        {
            let mut inner = db.lock_inner();
            // Key includes command + kind scope
            inner.entries.insert(
                frecency_key(Some("git"), SuggestionKind::Subcommand, "status"),
                FrecencyEntry {
                    stored_score: 5.0,
                    reference_secs: now_secs(),
                },
            );
        }

        let mut suggestions = vec![Suggestion {
            text: "status".into(),
            description: None,
            kind: SuggestionKind::Subcommand,
            source: SuggestionSource::Spec,
            score: 100,
            match_indices: vec![],
        }];

        db.boost_scores(&mut suggestions, Some("git"));
        // frecency ≈ 5.0, bonus ≈ 500
        assert!(
            suggestions[0].score > 500,
            "expected boosted score > 500, got {}",
            suggestions[0].score
        );
    }

    #[test]
    fn boost_scores_noop_for_unknown() {
        let db = FrecencyDb::empty();
        let mut suggestions = vec![Suggestion {
            text: "unknown cmd".into(),
            description: None,
            kind: SuggestionKind::History,
            source: SuggestionSource::History,
            score: 42,
            match_indices: vec![],
        }];
        db.boost_scores(&mut suggestions, None);
        assert_eq!(suggestions[0].score, 42);
    }

    #[test]
    fn context_aware_keys_are_distinct() {
        let db = FrecencyDb::empty();
        let git_key = frecency_key(Some("git"), SuggestionKind::Flag, "--help");
        let docker_key = frecency_key(Some("docker"), SuggestionKind::Flag, "--help");
        let cmd_key = frecency_key(None, SuggestionKind::Command, "git");

        db.record(&git_key);
        db.record(&git_key);
        db.record(&git_key);

        assert!(db.score(&git_key) > 2.5, "git --help should have score ~3");
        assert_eq!(
            db.score(&docker_key),
            0.0,
            "docker --help should be unaffected"
        );
        assert_eq!(db.score(&cmd_key), 0.0, "command-position git unaffected");
    }

    #[test]
    fn kind_scoping_prevents_same_command_collisions() {
        // Under `git`, a branch named `main` and a file named `main` should
        // have distinct frecency keys.
        let db = FrecencyDb::empty();
        let branch_key = frecency_key(Some("git"), SuggestionKind::GitBranch, "main");
        let file_key = frecency_key(Some("git"), SuggestionKind::FilePath, "main");
        let remote_key = frecency_key(Some("git"), SuggestionKind::GitRemote, "main");

        db.record(&branch_key);
        db.record(&branch_key);
        db.record(&branch_key);

        assert!(
            db.score(&branch_key) > 2.5,
            "git branch main should have score ~3"
        );
        assert_eq!(
            db.score(&file_key),
            0.0,
            "git file main should be unaffected"
        );
        assert_eq!(
            db.score(&remote_key),
            0.0,
            "git remote main should be unaffected"
        );
    }

    #[test]
    fn history_items_keyed_without_command_scope() {
        // History items should always use kind-only keys (no command prefix),
        // so recording from different buffer states produces the same key.
        let db = FrecencyDb::empty();

        let key_no_cmd = frecency_key(None, SuggestionKind::History, "git status");
        let key_with_cmd = frecency_key(Some("git"), SuggestionKind::History, "git status");

        // Verify they're different raw strings (command prefix differs)
        assert_ne!(key_no_cmd, key_with_cmd);

        // But boost_scores always uses None for history, so let's verify via boost
        db.record(&key_no_cmd);
        db.record(&key_no_cmd);
        db.record(&key_no_cmd);

        let mut suggestions = vec![Suggestion {
            text: "git status".into(),
            description: None,
            kind: SuggestionKind::History,
            source: SuggestionSource::History,
            score: 10,
            match_indices: vec![],
        }];

        // Even when called with Some("git"), history items should look up with None
        db.boost_scores(&mut suggestions, Some("git"));
        assert!(
            suggestions[0].score > 200,
            "history should be boosted via None-scoped key, got {}",
            suggestions[0].score
        );
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frecency.json");

        let db = FrecencyDb {
            inner: Mutex::new(FrecencyInner {
                entries: HashMap::new(),
                dirty_count: 0,
            }),
            path: Some(path.clone()),
        };
        db.record("ls -la");
        db.record("ls -la");
        db.record("cargo test");
        db.flush();

        // Load from same path
        let db2 = FrecencyDb::load_from(path);
        // ls -la was recorded twice in quick succession, score ≈ 2.0
        let ls_score = db2.score("ls -la");
        assert!(
            ls_score > 1.5,
            "expected ls -la score > 1.5, got {ls_score}"
        );
        let cargo_score = db2.score("cargo test");
        assert!(
            cargo_score > 0.5,
            "expected cargo test score > 0.5, got {cargo_score}"
        );
    }

    #[test]
    fn flush_independence_from_save_every() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frecency.json");

        let db = FrecencyDb {
            inner: Mutex::new(FrecencyInner {
                entries: HashMap::new(),
                dirty_count: 0,
            }),
            path: Some(path.clone()),
        };

        // Record fewer than SAVE_EVERY — should NOT auto-save
        db.record("only-one");
        assert!(!path.exists(), "should not auto-save before SAVE_EVERY");

        // But flush() should persist
        db.flush();
        assert!(path.exists(), "flush() should persist to disk");

        let db2 = FrecencyDb::load_from(path);
        assert!(db2.score("only-one") > 0.5, "flushed entry should load");
    }

    #[test]
    fn legacy_format_migration() {
        let legacy_json = r#"{
            "git push": {"frequency": 5, "last_used_secs": 1000000},
            "cargo test": {"frequency": 10, "last_used_secs": 2000000}
        }"#;

        let entries = FrecencyDb::deserialize_entries(legacy_json);
        assert_eq!(entries.len(), 2);

        let git = entries.get("git push").expect("git push should exist");
        assert_eq!(git.stored_score, 5.0);
        assert_eq!(git.reference_secs, 1000000);

        let cargo = entries.get("cargo test").expect("cargo test should exist");
        assert_eq!(cargo.stored_score, 10.0);
        assert_eq!(cargo.reference_secs, 2000000);
    }

    #[test]
    fn legacy_format_roundtrip_via_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frecency.json");

        // Write legacy format to disk
        let legacy = r#"{"ls": {"frequency": 3, "last_used_secs": 1700000000}}"#;
        std::fs::write(&path, legacy).unwrap();

        // Load should migrate
        let db = FrecencyDb::load_from(path.clone());
        let score = db.score("ls");
        assert!(score > 0.0, "migrated entry should have positive score");

        // Record something so dirty_count > 0, triggering flush to write
        db.record("ls");
        db.flush();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("stored_score"),
            "saved file should use new format"
        );
        assert!(
            !raw.contains("\"frequency\""),
            "saved file should not contain legacy fields"
        );
    }

    #[test]
    fn max_entries_pruning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frecency.json");

        let db = FrecencyDb {
            inner: Mutex::new(FrecencyInner {
                entries: HashMap::new(),
                dirty_count: 0,
            }),
            path: Some(path.clone()),
        };

        // Insert more than MAX_ENTRIES
        {
            let mut inner = db.lock_inner();
            let now = now_secs();
            for i in 0..MAX_ENTRIES + 50 {
                inner.entries.insert(
                    format!("entry-{i}"),
                    FrecencyEntry {
                        stored_score: i as f64,
                        reference_secs: now,
                    },
                );
            }
            inner.dirty_count = 1; // mark dirty for flush
        }

        db.flush();

        let db2 = FrecencyDb::load_from(path);
        let inner = db2.lock_inner();
        assert!(
            inner.entries.len() <= MAX_ENTRIES,
            "should prune to MAX_ENTRIES, got {}",
            inner.entries.len()
        );
        // The lowest-scoring entries (0..49) should have been evicted
        assert!(
            inner.entries.contains_key("entry-1049"),
            "high-scoring entry should survive"
        );
        assert!(
            !inner.entries.contains_key("entry-0"),
            "lowest-scoring entry should be evicted"
        );
    }

    #[test]
    fn exponential_decay_two_half_lives() {
        let db = FrecencyDb::empty();
        {
            let mut inner = db.lock_inner();
            // 6 days ago = two half-lives
            let six_days_ago = now_secs() - (144 * 3600);
            inner.entries.insert(
                "ancient".into(),
                FrecencyEntry {
                    stored_score: 8.0,
                    reference_secs: six_days_ago,
                },
            );
        }

        let s = db.score("ancient");
        // After two half-lives: 8.0 / 4.0 = 2.0
        assert!(
            (s - 2.0).abs() < 0.2,
            "expected score near 2.0 after two half-lives, got {s}"
        );
    }

    #[test]
    fn concurrent_record_and_score_no_deadlock() {
        // Stress test: many threads simultaneously record() (which triggers
        // disk saves every SAVE_EVERY calls) while others score(). The save
        // path must release the mutex before touching disk so score() is not
        // blocked behind a slow filesystem; this test verifies no thread
        // deadlocks and all expected entries land in the db.
        use std::sync::Arc;
        use std::thread;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frecency.json");
        let db = Arc::new(FrecencyDb {
            inner: Mutex::new(FrecencyInner {
                entries: HashMap::new(),
                dirty_count: 0,
            }),
            path: Some(path),
        });

        let mut handles = vec![];
        for t in 0..4 {
            let db = Arc::clone(&db);
            handles.push(thread::spawn(move || {
                for i in 0..50 {
                    let key = format!("key_{t}_{i}");
                    db.record(&key);
                    // Reading must succeed concurrently with peer writers'
                    // disk saves; if the lock were held across I/O this
                    // would serialize and slow down dramatically.
                    let _ = db.score(&key);
                }
            }));
        }
        for h in handles {
            h.join().expect("worker thread panicked");
        }

        db.flush();

        // Sanity: every key recorded should have a positive score, and a
        // sampled subset must be present.
        for t in 0..4 {
            for i in [0_usize, 49] {
                let key = format!("key_{t}_{i}");
                assert!(db.score(&key) > 0.0, "expected positive frecency for {key}");
            }
        }
    }

    #[test]
    fn dirty_count_not_reset_on_failed_save() {
        // A db with an invalid path (directory that can't be created)
        let db = FrecencyDb {
            inner: Mutex::new(FrecencyInner {
                entries: HashMap::new(),
                dirty_count: 0,
            }),
            path: Some(PathBuf::from("/dev/null/impossible/frecency.json")),
        };

        // Record SAVE_EVERY times to trigger auto-save attempt
        for _ in 0..SAVE_EVERY {
            db.record("test");
        }

        // dirty_count should NOT have been reset since save failed
        let inner = db.lock_inner();
        assert!(
            inner.dirty_count >= SAVE_EVERY,
            "dirty_count should not reset on failed save, got {}",
            inner.dirty_count
        );
    }

    #[test]
    fn versioned_envelope_roundtrip() {
        let envelope = r#"{
            "version": 1,
            "entries": {
                "git push": {"stored_score": 5.0, "reference_secs": 1700000000}
            }
        }"#;
        let entries = FrecencyDb::deserialize_entries(envelope);
        assert_eq!(entries.len(), 1);
        let e = entries.get("git push").expect("key should exist");
        assert_eq!(e.stored_score, 5.0);
        assert_eq!(e.reference_secs, 1700000000);
    }

    #[test]
    fn future_version_treated_as_empty() {
        let future = r#"{
            "version": 99999,
            "entries": {
                "git push": {"stored_score": 5.0, "reference_secs": 1700000000}
            }
        }"#;
        let entries = FrecencyDb::deserialize_entries(future);
        assert!(
            entries.is_empty(),
            "future version must be treated as empty; got {entries:?}"
        );
    }

    #[test]
    fn save_refuses_to_overwrite_future_version() {
        // A future release writes version 99; our downgrade must not clobber it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frecency.json");
        let future_raw = r#"{"version":99,"entries":{}}"#;
        std::fs::write(&path, future_raw).unwrap();

        let db = FrecencyDb {
            inner: Mutex::new(FrecencyInner {
                entries: HashMap::new(),
                dirty_count: 0,
            }),
            path: Some(path.clone()),
        };
        db.record("foo");
        db.flush();

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            on_disk, future_raw,
            "save must refuse to overwrite a newer-version file"
        );
    }

    #[test]
    fn merge_on_save_unions_two_peers() {
        // Two independent processes touch the same frecency file. The
        // last-to-save must preserve the peer's entries, not clobber them.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frecency.json");

        // Peer A records many keys and saves.
        let db_a = FrecencyDb {
            inner: Mutex::new(FrecencyInner {
                entries: HashMap::new(),
                dirty_count: 0,
            }),
            path: Some(path.clone()),
        };
        for i in 0..30 {
            db_a.record(&format!("peer_a_{i}"));
        }
        db_a.flush();
        assert!(path.exists(), "A should have written the file");

        // Peer B starts from an empty in-memory state (does NOT load from
        // disk) — models a second terminal that started before A's entries
        // landed — records its own keys, then saves.
        let db_b = FrecencyDb {
            inner: Mutex::new(FrecencyInner {
                entries: HashMap::new(),
                dirty_count: 0,
            }),
            path: Some(path.clone()),
        };
        for i in 0..30 {
            db_b.record(&format!("peer_b_{i}"));
        }
        db_b.flush();

        // After both saves, reload from disk and verify both peers' keys
        // are present — the merge must have unioned them.
        let db_final = FrecencyDb::load_from(path);
        for i in 0..30 {
            let a_key = format!("peer_a_{i}");
            let b_key = format!("peer_b_{i}");
            assert!(
                db_final.score(&a_key) > 0.0,
                "peer A key {a_key} must survive merge"
            );
            assert!(
                db_final.score(&b_key) > 0.0,
                "peer B key {b_key} must survive merge"
            );
        }
    }

    #[test]
    fn merge_picks_max_decayed_score_per_key() {
        // For a key both peers know about, the higher decayed-score entry wins.
        let now = now_secs();
        let mut disk = HashMap::new();
        disk.insert(
            "shared".to_string(),
            FrecencyEntry {
                stored_score: 2.0,
                reference_secs: now,
            },
        );
        let mut mem = HashMap::new();
        mem.insert(
            "shared".to_string(),
            FrecencyEntry {
                stored_score: 10.0,
                reference_secs: now,
            },
        );

        let merged = FrecencyDb::merge_snapshots(disk, mem, now);
        let e = merged.get("shared").unwrap();
        assert_eq!(
            e.stored_score, 10.0,
            "merge must keep the larger decayed score"
        );
    }

    #[test]
    fn merge_compares_on_decayed_score_not_raw() {
        // Ancient-but-large vs fresh-but-small. Decayed, the fresh one wins.
        let now = now_secs();
        let one_week_ago = now - 7 * 24 * 3600; // >2 half-lives
        let mut disk = HashMap::new();
        disk.insert(
            "k".to_string(),
            FrecencyEntry {
                stored_score: 100.0,
                reference_secs: one_week_ago,
            },
        );
        let mut mem = HashMap::new();
        mem.insert(
            "k".to_string(),
            FrecencyEntry {
                stored_score: 50.0,
                reference_secs: now,
            },
        );
        let merged = FrecencyDb::merge_snapshots(disk, mem, now);
        let e = merged.get("k").unwrap();
        // Disk: 100 / 2^(168/72) ~= 100 * 0.195 ≈ 19.5
        // Mem:  50 (fresh)
        // Mem must win.
        assert_eq!(
            e.reference_secs, now,
            "mem entry (higher decayed score) must win"
        );
    }

    #[test]
    fn score_clamped_to_max_stored_score() {
        // Construct an entry at the clamp ceiling, record once more, and
        // verify stored_score does not exceed MAX_STORED_SCORE.
        let db = FrecencyDb::empty();
        {
            let mut inner = db.lock_inner();
            inner.entries.insert(
                "hot".into(),
                FrecencyEntry {
                    stored_score: MAX_STORED_SCORE,
                    reference_secs: now_secs(),
                },
            );
        }
        db.record("hot");
        let inner = db.lock_inner();
        let e = inner.entries.get("hot").unwrap();
        assert!(
            e.stored_score <= MAX_STORED_SCORE,
            "stored_score must be clamped; got {}",
            e.stored_score
        );
        assert!(
            e.stored_score.is_finite(),
            "stored_score must remain finite"
        );
    }

    #[test]
    fn xdg_state_home_respected() {
        // When XDG_STATE_HOME is set, resolve_state_path must use it.
        let old = std::env::var("XDG_STATE_HOME").ok();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: single-threaded test, no other threads touch env here.
        std::env::set_var("XDG_STATE_HOME", dir.path());
        let p = resolve_state_path().expect("path should resolve");
        assert!(
            p.starts_with(dir.path()),
            "state path must live under XDG_STATE_HOME; got {}",
            p.display()
        );
        assert!(
            p.ends_with("ghost-complete/frecency.json"),
            "state path must end with ghost-complete/frecency.json; got {}",
            p.display()
        );
        match old {
            Some(v) => std::env::set_var("XDG_STATE_HOME", v),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn save_refuses_to_overwrite_unreadable_existing_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frecency.json");
        let original = br#"{"version":1,"entries":{"preexisting":{"stored_score":7.5,"reference_secs":1700000000}}}"#;
        std::fs::write(&path, original).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();

        let mut snapshot = HashMap::new();
        snapshot.insert(
            "new_entry".to_string(),
            FrecencyEntry {
                stored_score: 1.0,
                reference_secs: now_secs(),
            },
        );

        let ok = FrecencyDb::save_snapshot(snapshot, &Some(path.clone()));
        assert!(!ok, "save must refuse to overwrite an unreadable file");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            bytes.as_slice(),
            original.as_slice(),
            "original bytes must survive the refused save"
        );
    }
}
