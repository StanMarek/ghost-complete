//! Frecency-weighted scoring for suggestions.
//!
//! Frequently **and** recently used completions rank higher. Uses exponential
//! decay with a half-life of 72 hours (3 days) — the full usage history is
//! compressed into a single f64 per entry.
//!
//! Keys are command-scoped: an argument completion under `git` is stored as
//! `git\0--help`, distinct from `docker\0--help`. Command-position completions
//! (where there is no parent command) use the raw text as key.
//!
//! Storage lives at `~/.config/ghost-complete/frecency.json`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::types::Suggestion;

/// File name within the config directory.
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

/// Build a command-scoped frecency key.
///
/// Argument completions are keyed as `"command\0text"` so that `--help`
/// under `git` doesn't pollute `docker`'s ranking. Command-position
/// completions (no parent command) use the raw text.
pub fn frecency_key(command: Option<&str>, text: &str) -> String {
    match command {
        Some(cmd) if !cmd.is_empty() => format!("{cmd}{KEY_SEP}{text}"),
        _ => text.to_string(),
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

    /// Load from the default config directory. Returns an empty database on
    /// any I/O or parse error so callers never have to handle failures.
    pub fn load() -> Self {
        let path = gc_config::config_dir().map(|d| d.join(FRECENCY_FILE));
        let entries = match &path {
            Some(p) if p.exists() => match std::fs::read_to_string(p) {
                Ok(s) => Self::deserialize_entries(&s),
                Err(e) => {
                    tracing::debug!("frecency file unreadable: {e}");
                    HashMap::new()
                }
            },
            _ => HashMap::new(),
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

    /// Deserialize entries, migrating from the legacy `{frequency, last_used_secs}`
    /// format if the current format fails. This ensures users upgrading from
    /// pre-v0.5.0 don't lose their learned ranking data.
    fn deserialize_entries(json: &str) -> HashMap<String, FrecencyEntry> {
        // Try current format first
        if let Ok(map) = serde_json::from_str::<HashMap<String, FrecencyEntry>>(json) {
            return map;
        }

        // Try legacy format and migrate
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

    /// Persist the current state to disk. Prunes to `MAX_ENTRIES` by evicting
    /// entries with the lowest actual scores. Uses atomic write (tmp + rename).
    ///
    /// Note: this is called while the Mutex is held. On NVMe this is sub-ms;
    /// on networked home dirs it could spike. Acceptable for v1 — a future
    /// optimization could clone entries and save outside the critical section.
    ///
    /// Returns `true` on success, `false` on any failure.
    fn save_inner(inner: &FrecencyInner, path: &Option<PathBuf>) -> bool {
        let Some(ref path) = path else { return true };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!("frecency dir creation failed: {e}");
                return false;
            }
        }

        let now = now_secs();

        // Prune if over the cap — keep the highest-scoring entries
        let entries_to_save = if inner.entries.len() > MAX_ENTRIES {
            let mut scored: Vec<_> = inner
                .entries
                .iter()
                .map(|(k, v)| (k.clone(), v.clone(), v.actual_score(now)))
                .collect();
            scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(MAX_ENTRIES);
            scored
                .into_iter()
                .map(|(k, v, _)| (k, v))
                .collect::<HashMap<_, _>>()
        } else {
            inner.entries.clone()
        };

        let json = match serde_json::to_string_pretty(&entries_to_save) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("frecency serialize error: {e}");
                return false;
            }
        };

        let tmp = path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, &json) {
            tracing::warn!("frecency save error (write tmp): {e}");
            return false;
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            tracing::warn!("frecency save error (rename): {e}");
            // Clean up stale temp file
            let _ = std::fs::remove_file(&tmp);
            return false;
        }

        true
    }

    /// Record a completion acceptance — decays existing score and adds 1.0.
    /// Batches disk writes: flushes every `SAVE_EVERY` records.
    pub fn record(&self, key: &str) {
        let mut inner = self.lock_inner();
        let now = now_secs();

        let entry = inner
            .entries
            .entry(key.to_string())
            .or_insert(FrecencyEntry {
                stored_score: 0.0,
                reference_secs: now,
            });

        // Decay existing score to current time, then add 1.0
        let actual = entry.actual_score(now);
        entry.stored_score = actual + 1.0;
        entry.reference_secs = now;

        inner.dirty_count += 1;
        if inner.dirty_count >= SAVE_EVERY && Self::save_inner(&inner, &self.path) {
            inner.dirty_count = 0;
        }
    }

    /// Flush any unsaved records to disk. Call on proxy shutdown.
    pub fn flush(&self) {
        let mut inner = self.lock_inner();
        if inner.dirty_count > 0 && Self::save_inner(&inner, &self.path) {
            inner.dirty_count = 0;
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
            let key = frecency_key(command, &suggestion.text);
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
            // Key includes command scope
            inner.entries.insert(
                frecency_key(Some("git"), "status"),
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
        let git_key = frecency_key(Some("git"), "--help");
        let docker_key = frecency_key(Some("docker"), "--help");
        let cmd_key = frecency_key(None, "git");

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
            !raw.contains("frequency"),
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
}
