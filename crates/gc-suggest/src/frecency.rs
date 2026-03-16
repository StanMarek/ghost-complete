//! Frecency-weighted scoring for history suggestions.
//!
//! Frequently **and** recently used commands rank higher. The score formula
//! is `frequency * recency_weight` where the recency weight decays with a
//! half-life of approximately one week (168 hours).
//!
//! Storage lives at `~/.config/ghost-complete/frecency.json`.

use std::collections::HashMap;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::types::Suggestion;

/// File name within the config directory.
const FRECENCY_FILE: &str = "frecency.json";

/// Recency half-life in hours (one week).
const HALF_LIFE_HOURS: f64 = 168.0;

/// Maximum entries to persist. Older/less-used entries are evicted on save.
const MAX_ENTRIES: usize = 1000;

/// A single entry tracking how often and how recently a command was used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrecencyEntry {
    pub frequency: u32,
    /// Seconds since the Unix epoch — `SystemTime` doesn't implement Serde
    /// traits, so we store the raw value.
    pub last_used_secs: u64,
}

impl FrecencyEntry {
    fn last_used(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(self.last_used_secs)
    }
}

/// In-memory frecency database backed by a JSON file on disk.
#[derive(Debug, Clone)]
pub struct FrecencyDb {
    entries: HashMap<String, FrecencyEntry>,
    /// `None` when running in tests with no real config directory.
    path: Option<std::path::PathBuf>,
    /// Number of unsaved record() calls. Flushes after this many.
    dirty_count: u32,
}

impl FrecencyDb {
    /// Load from the default config directory. Returns an empty database on
    /// any I/O or parse error so callers never have to handle failures.
    pub fn load() -> Self {
        let path = gc_config::config_dir().map(|d| d.join(FRECENCY_FILE));
        let entries = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<HashMap<String, FrecencyEntry>>(&s).ok())
            .unwrap_or_default();
        Self {
            entries,
            path,
            dirty_count: 0,
        }
    }

    /// Load from a specific path (useful for tests).
    #[cfg(test)]
    pub fn load_from(path: std::path::PathBuf) -> Self {
        let entries = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, FrecencyEntry>>(&s).ok())
            .unwrap_or_default();
        Self {
            entries,
            path: Some(path),
            dirty_count: 0,
        }
    }

    /// Create an empty database that never touches disk.
    pub fn empty() -> Self {
        Self {
            entries: HashMap::new(),
            path: None,
            dirty_count: 0,
        }
    }

    /// Persist the current state to disk. Prunes to `MAX_ENTRIES` by evicting
    /// entries with the lowest frecency scores. Errors are logged but not
    /// propagated — frecency is best-effort.
    pub fn save(&self) {
        let Some(ref path) = self.path else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // Prune if over the cap — keep the highest-scoring entries
        let entries_to_save = if self.entries.len() > MAX_ENTRIES {
            let mut scored: Vec<_> = self
                .entries
                .iter()
                .map(|(k, v)| {
                    let hours = SystemTime::now()
                        .duration_since(v.last_used())
                        .unwrap_or_default()
                        .as_secs_f64()
                        / 3600.0;
                    let score = f64::from(v.frequency) / (1.0 + hours / HALF_LIFE_HOURS);
                    (k.clone(), v.clone(), score)
                })
                .collect();
            scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(MAX_ENTRIES);
            scored
                .into_iter()
                .map(|(k, v, _)| (k, v))
                .collect::<HashMap<_, _>>()
        } else {
            self.entries.clone()
        };

        match serde_json::to_string_pretty(&entries_to_save) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    tracing::debug!("frecency save error: {e}");
                }
            }
            Err(e) => tracing::debug!("frecency serialize error: {e}"),
        }
    }

    /// Batch-save threshold. Saves to disk every N record() calls to avoid
    /// blocking the hot path with synchronous I/O on every keystroke.
    const SAVE_EVERY: u32 = 10;

    /// Record a command usage — increments frequency, updates timestamp.
    /// Batches disk writes: flushes every 10 records.
    pub fn record(&mut self, command: &str) {
        let now_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let entry = self
            .entries
            .entry(command.to_string())
            .or_insert(FrecencyEntry {
                frequency: 0,
                last_used_secs: now_secs,
            });
        entry.frequency += 1;
        entry.last_used_secs = now_secs;

        self.dirty_count += 1;
        if self.dirty_count >= Self::SAVE_EVERY {
            self.save();
            self.dirty_count = 0;
        }
    }

    /// Flush any unsaved records to disk. Call on proxy shutdown.
    pub fn flush(&mut self) {
        if self.dirty_count > 0 {
            self.save();
            self.dirty_count = 0;
        }
    }

    /// Compute the frecency score for a command.
    ///
    /// Returns `0.0` for unknown commands. The formula is:
    /// `frequency * (1.0 / (1.0 + hours_since_last_use / 168.0))`
    pub fn score(&self, command: &str) -> f64 {
        let Some(entry) = self.entries.get(command) else {
            return 0.0;
        };

        let hours_since = SystemTime::now()
            .duration_since(entry.last_used())
            .unwrap_or_default()
            .as_secs_f64()
            / 3600.0;

        let recency_weight = 1.0 / (1.0 + hours_since / HALF_LIFE_HOURS);
        f64::from(entry.frequency) * recency_weight
    }

    /// Apply a frecency bonus to a suggestion's score. The frecency value is
    /// scaled and added to the existing `u32` score so that the popup ordering
    /// naturally favours frequently/recently used commands.
    pub fn boost_score(&self, suggestion: &mut Suggestion) {
        let frecency = self.score(&suggestion.text);
        if frecency > 0.0 {
            // Scale frecency into a bonus that meaningfully affects nucleo's
            // u32 score range. A multiplier of 100 means ~10 uses within the
            // last week adds ~1000 to the score.
            let bonus = (frecency * 100.0).min(u32::MAX as f64) as u32;
            suggestion.score = suggestion.score.saturating_add(bonus);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{SuggestionKind, SuggestionSource};
    use std::time::Duration;

    #[test]
    fn empty_db_returns_zero_score() {
        let db = FrecencyDb::empty();
        assert_eq!(db.score("anything"), 0.0);
    }

    #[test]
    fn record_increments_frequency() {
        let mut db = FrecencyDb::empty();
        db.record("git push");
        assert_eq!(db.entries["git push"].frequency, 1);
        db.record("git push");
        assert_eq!(db.entries["git push"].frequency, 2);
    }

    #[test]
    fn score_calculation_recent() {
        // A command used just now should have recency_weight ≈ 1.0,
        // so score ≈ frequency.
        let mut db = FrecencyDb::empty();
        let now_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        db.entries.insert(
            "cargo build".into(),
            FrecencyEntry {
                frequency: 10,
                last_used_secs: now_secs,
            },
        );

        let s = db.score("cargo build");
        // recency_weight ≈ 1.0 for something used just now
        assert!(s > 9.5, "expected score near 10.0, got {s}");
        assert!(s <= 10.0, "expected score <= 10.0, got {s}");
    }

    #[test]
    fn score_calculation_old() {
        // A command used exactly one week ago should have recency_weight = 0.5,
        // so score ≈ frequency * 0.5.
        let mut db = FrecencyDb::empty();
        let one_week_ago = SystemTime::now()
            .checked_sub(Duration::from_secs(168 * 3600))
            .unwrap();
        let secs = one_week_ago
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        db.entries.insert(
            "old command".into(),
            FrecencyEntry {
                frequency: 10,
                last_used_secs: secs,
            },
        );

        let s = db.score("old command");
        // recency_weight = 1/(1+168/168) = 0.5, so score ≈ 5.0
        assert!((s - 5.0).abs() < 0.1, "expected score near 5.0, got {s}");
    }

    #[test]
    fn boost_score_adds_bonus() {
        let mut db = FrecencyDb::empty();
        let now_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        db.entries.insert(
            "git status".into(),
            FrecencyEntry {
                frequency: 5,
                last_used_secs: now_secs,
            },
        );

        let mut suggestion = Suggestion {
            text: "git status".into(),
            description: None,
            kind: SuggestionKind::History,
            source: SuggestionSource::History,
            score: 100,
            match_indices: vec![],
        };

        db.boost_score(&mut suggestion);
        // frecency ≈ 5.0 * 1.0 = 5.0, bonus ≈ 500
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

        let mut db = FrecencyDb {
            entries: HashMap::new(),
            path: Some(path.clone()),
            dirty_count: 0,
        };
        db.record("ls -la");
        db.record("ls -la");
        db.record("cargo test");
        db.flush(); // force write (batched saves won't trigger with only 3 records)

        // Load from same path
        let db2 = FrecencyDb::load_from(path);
        assert_eq!(db2.entries["ls -la"].frequency, 2);
        assert_eq!(db2.entries["cargo test"].frequency, 1);
        // Score should be positive for known commands
        assert!(db2.score("ls -la") > 0.0);
        assert!(db2.score("cargo test") > 0.0);
    }
}
