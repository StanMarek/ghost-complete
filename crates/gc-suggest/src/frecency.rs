//! Frecency-weighted scoring for suggestions.
//!
//! Frequently **and** recently used completions rank higher. Uses exponential
//! decay with a half-life of 72 hours (3 days) — the full usage history is
//! compressed into a single f64 per entry.
//!
//! Storage lives at `~/.config/ghost-complete/frecency.json`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
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

impl FrecencyDb {
    /// Load from the default config directory. Returns an empty database on
    /// any I/O or parse error so callers never have to handle failures.
    pub fn load() -> Self {
        let path = gc_config::config_dir().map(|d| d.join(FRECENCY_FILE));
        let entries = match &path {
            Some(p) if p.exists() => match std::fs::read_to_string(p) {
                Ok(s) => match serde_json::from_str::<HashMap<String, FrecencyEntry>>(&s) {
                    Ok(map) => map,
                    Err(e) => {
                        tracing::warn!("frecency data corrupt, starting fresh: {e}");
                        HashMap::new()
                    }
                },
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
            .and_then(|s| serde_json::from_str::<HashMap<String, FrecencyEntry>>(&s).ok())
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

    /// Persist the current state to disk. Prunes to `MAX_ENTRIES` by evicting
    /// entries with the lowest actual scores. Uses atomic write (tmp + rename).
    fn save_inner(inner: &FrecencyInner, path: &Option<PathBuf>) {
        let Some(ref path) = path else { return };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!("frecency dir creation failed: {e}");
                return;
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

        match serde_json::to_string_pretty(&entries_to_save) {
            Ok(json) => {
                let tmp = path.with_extension("json.tmp");
                if let Err(e) = std::fs::write(&tmp, &json) {
                    tracing::warn!("frecency save error (write tmp): {e}");
                    return;
                }
                if let Err(e) = std::fs::rename(&tmp, path) {
                    tracing::warn!("frecency save error (rename): {e}");
                }
            }
            Err(e) => tracing::debug!("frecency serialize error: {e}"),
        }
    }

    /// Record a completion acceptance — decays existing score and adds 1.0.
    /// Batches disk writes: flushes every 10 records.
    pub fn record(&self, text: &str) {
        let mut inner = self.inner.lock().unwrap();
        let now = now_secs();

        let entry = inner
            .entries
            .entry(text.to_string())
            .or_insert(FrecencyEntry {
                stored_score: 0.0,
                reference_secs: now,
            });

        // Decay existing score to current time, then add 1.0
        let actual = entry.actual_score(now);
        entry.stored_score = actual + 1.0;
        entry.reference_secs = now;

        inner.dirty_count += 1;
        if inner.dirty_count >= SAVE_EVERY {
            Self::save_inner(&inner, &self.path);
            inner.dirty_count = 0;
        }
    }

    /// Flush any unsaved records to disk. Call on proxy shutdown.
    pub fn flush(&self) {
        let mut inner = self.inner.lock().unwrap();
        if inner.dirty_count > 0 {
            Self::save_inner(&inner, &self.path);
            inner.dirty_count = 0;
        }
    }

    /// Compute the frecency score for a completion text.
    /// Returns `0.0` for unknown entries.
    pub fn score(&self, text: &str) -> f64 {
        let inner = self.inner.lock().unwrap();
        inner
            .entries
            .get(text)
            .map(|e| e.actual_score(now_secs()))
            .unwrap_or(0.0)
    }

    /// Apply a frecency bonus to a suggestion's score. The frecency value is
    /// scaled and added to the existing `u32` score so that the popup ordering
    /// naturally favours frequently/recently used completions.
    pub fn boost_score(&self, suggestion: &mut Suggestion) {
        let inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.entries.get(&suggestion.text) {
            let frecency = entry.actual_score(now_secs());
            if frecency > 0.0 {
                // Scale frecency into a bonus that meaningfully affects nucleo's
                // u32 score range. A multiplier of 100 means ~10 recent uses
                // adds ~1000 to the score.
                let bonus = (frecency * 100.0).min(u32::MAX as f64) as u32;
                suggestion.score = suggestion.score.saturating_add(bonus);
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
            let mut inner = db.inner.lock().unwrap();
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
            let mut inner = db.inner.lock().unwrap();
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
    fn boost_score_adds_bonus() {
        let db = FrecencyDb::empty();
        {
            let mut inner = db.inner.lock().unwrap();
            inner.entries.insert(
                "git status".into(),
                FrecencyEntry {
                    stored_score: 5.0,
                    reference_secs: now_secs(),
                },
            );
        }

        let mut suggestion = Suggestion {
            text: "git status".into(),
            description: None,
            kind: SuggestionKind::History,
            source: SuggestionSource::History,
            score: 100,
            match_indices: vec![],
        };

        db.boost_score(&mut suggestion);
        // frecency ≈ 5.0, bonus ≈ 500
        assert!(
            suggestion.score > 500,
            "expected boosted score > 500, got {}",
            suggestion.score
        );
    }

    #[test]
    fn boost_score_noop_for_unknown() {
        let db = FrecencyDb::empty();
        let mut suggestion = Suggestion {
            text: "unknown cmd".into(),
            description: None,
            kind: SuggestionKind::History,
            source: SuggestionSource::History,
            score: 42,
            match_indices: vec![],
        };
        db.boost_score(&mut suggestion);
        assert_eq!(suggestion.score, 42);
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
    fn exponential_decay_two_half_lives() {
        let db = FrecencyDb::empty();
        {
            let mut inner = db.inner.lock().unwrap();
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
}
