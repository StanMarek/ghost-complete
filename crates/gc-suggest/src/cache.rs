use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::types::Suggestion;

/// Cache key for generator results.
///
/// Composed of `(spec_name, resolved_command_argv, cwd_if_cache_by_directory)`.
/// Uses the fully resolved command (post-substitution for `script_template`).
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct CacheKey {
    spec_name: String,
    resolved_argv: Vec<String>,
    cwd: Option<String>,
}

impl CacheKey {
    pub fn new(spec_name: &str, argv: &[&str], cwd: Option<&Path>) -> Self {
        Self {
            spec_name: spec_name.into(),
            resolved_argv: argv.iter().map(|s| s.to_string()).collect(),
            cwd: cwd.map(|p| p.to_string_lossy().to_string()),
        }
    }

    pub fn from_strings(spec_name: &str, argv: &[String], cwd: Option<&Path>) -> Self {
        Self {
            spec_name: spec_name.into(),
            resolved_argv: argv.to_vec(),
            cwd: cwd.map(|p| p.to_string_lossy().to_string()),
        }
    }
}

struct CacheEntry {
    suggestions: Vec<Suggestion>,
    expires_at: Instant,
    inserted_at: Instant,
}

impl CacheEntry {
    fn is_expired(&self, now: Instant) -> bool {
        now >= self.expires_at
    }
}

/// Threshold above which `insert()` triggers an eviction sweep. Script-template
/// generator keys embed user input (e.g. `git log --author={current_token}`),
/// so an interactive typing session can manufacture unbounded distinct keys.
/// See audit MED-20.
const CACHE_SWEEP_THRESHOLD: usize = 500;

/// In-memory TTL cache for generator results.
///
/// Thread-safe via internal `Mutex`. Entries expire after their individual TTL.
pub struct GeneratorCache {
    entries: Mutex<HashMap<CacheKey, CacheEntry>>,
}

impl GeneratorCache {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Look up a cache entry. Returns `None` if the key is absent or expired.
    /// Expired entries are removed on access to prevent unbounded memory growth.
    pub fn get(&self, key: &CacheKey) -> Option<Vec<Suggestion>> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        match entries.get(key) {
            Some(entry) if !entry.is_expired(now) => Some(entry.suggestions.clone()),
            Some(_) => {
                // Expired — evict
                entries.remove(key);
                None
            }
            None => None,
        }
    }

    /// Insert (or replace) a cache entry with the given TTL.
    ///
    /// When the post-insert size exceeds [`CACHE_SWEEP_THRESHOLD`] this also
    /// runs a sweep: first dropping every expired entry, then — if still over
    /// the threshold — dropping the oldest entries by `inserted_at` until the
    /// size is back at the threshold. This bounds memory in the face of
    /// script-template keys whose argv embeds user input.
    pub fn insert(&self, key: CacheKey, suggestions: Vec<Suggestion>, ttl: Duration) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        entries.insert(
            key,
            CacheEntry {
                suggestions,
                expires_at: now + ttl,
                inserted_at: now,
            },
        );
        Self::sweep_if_oversized(&mut entries, now);
    }

    fn sweep_if_oversized(entries: &mut HashMap<CacheKey, CacheEntry>, now: Instant) {
        if entries.len() <= CACHE_SWEEP_THRESHOLD {
            return;
        }
        // Pass 1: drop everything that has already expired.
        entries.retain(|_, entry| !entry.is_expired(now));
        if entries.len() <= CACHE_SWEEP_THRESHOLD {
            return;
        }
        // Pass 2: still oversize — drop the oldest entries by `inserted_at`
        // until we're back at the threshold.
        let excess = entries.len() - CACHE_SWEEP_THRESHOLD;
        let mut by_age: Vec<(Instant, CacheKey)> = entries
            .iter()
            .map(|(k, v)| (v.inserted_at, k.clone()))
            .collect();
        by_age.sort_by_key(|(t, _)| *t);
        for (_, key) in by_age.into_iter().take(excess) {
            entries.remove(&key);
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for GeneratorCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::types::{Suggestion, SuggestionSource};

    fn make_suggestions() -> Vec<Suggestion> {
        vec![Suggestion {
            text: "test".into(),
            source: SuggestionSource::Spec,
            ..Default::default()
        }]
    }

    #[test]
    fn test_cache_hit() {
        let cache = GeneratorCache::new();
        let key = CacheKey::new("brew", &["brew", "formulae"], Some(Path::new("/tmp")));
        cache.insert(key.clone(), make_suggestions(), Duration::from_secs(300));
        let result = cache.get(&key);
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 1);
    }

    #[test]
    fn test_cache_miss() {
        let cache = GeneratorCache::new();
        let key = CacheKey::new("brew", &["brew", "formulae"], Some(Path::new("/tmp")));
        assert!(cache.get(&key).is_none());
    }

    #[test]
    fn test_cache_expired() {
        let cache = GeneratorCache::new();
        let key = CacheKey::new("brew", &["brew", "formulae"], Some(Path::new("/tmp")));
        cache.insert(key.clone(), make_suggestions(), Duration::from_secs(0));
        std::thread::sleep(Duration::from_millis(10));
        assert!(cache.get(&key).is_none());
    }

    #[test]
    fn test_cache_different_cwd() {
        let cache = GeneratorCache::new();
        let key1 = CacheKey::new("brew", &["brew", "formulae"], Some(Path::new("/tmp")));
        let key2 = CacheKey::new("brew", &["brew", "formulae"], Some(Path::new("/home")));
        cache.insert(key1.clone(), make_suggestions(), Duration::from_secs(300));
        assert!(cache.get(&key1).is_some());
        assert!(cache.get(&key2).is_none());
    }

    #[test]
    fn test_cache_different_argv() {
        let cache = GeneratorCache::new();
        let key1 = CacheKey::new(
            "docker",
            &["docker", "ps", "--format", "json"],
            Some(Path::new("/tmp")),
        );
        let key2 = CacheKey::new(
            "docker",
            &["docker", "images", "--format", "json"],
            Some(Path::new("/tmp")),
        );
        cache.insert(key1.clone(), make_suggestions(), Duration::from_secs(300));
        assert!(cache.get(&key1).is_some());
        assert!(cache.get(&key2).is_none());
    }

    #[test]
    fn test_cache_sweep_drops_expired_on_oversize_insert() {
        // 400 expired entries, then 200 fresh ones. Once the cache crosses
        // the 500-entry threshold the sweep should drop every expired entry,
        // leaving exactly the 200 fresh ones behind.
        let cache = GeneratorCache::new();

        for i in 0..400 {
            let key = CacheKey::new("spec", &["cmd", &format!("expired_{i}")], None);
            cache.insert(key, make_suggestions(), Duration::from_nanos(1));
        }
        // Ensure the short-TTL entries are observably expired before the
        // sweep runs (Instant resolution is sub-microsecond on modern OSes,
        // but a small sleep eliminates any flakiness).
        std::thread::sleep(Duration::from_millis(2));

        for i in 0..200 {
            let key = CacheKey::new("spec", &["cmd", &format!("fresh_{i}")], None);
            cache.insert(key, make_suggestions(), Duration::from_secs(300));
        }

        assert_eq!(
            cache.len(),
            200,
            "expired entries must be evicted by the insert-time sweep"
        );

        // Every fresh entry must still be there.
        for i in 0..200 {
            let key = CacheKey::new("spec", &["cmd", &format!("fresh_{i}")], None);
            assert!(cache.get(&key).is_some(), "fresh_{i} should be retained");
        }
    }

    #[test]
    fn test_cache_sweep_lru_drops_oldest_when_no_expired() {
        // 600 entries with future TTLs — none are expired, so the sweep must
        // fall back to LRU-by-`inserted_at`. End state: exactly the 500 most
        // recent entries.
        let cache = GeneratorCache::new();

        for i in 0..500 {
            let key = CacheKey::new("spec", &["cmd", &format!("k_{i}")], None);
            cache.insert(key, make_suggestions(), Duration::from_secs(300));
        }
        // Force a clear `inserted_at` gap so the LRU drop is deterministic:
        // every entry from the second batch is strictly newer than every
        // entry from the first batch.
        std::thread::sleep(Duration::from_millis(2));
        for i in 500..600 {
            let key = CacheKey::new("spec", &["cmd", &format!("k_{i}")], None);
            cache.insert(key, make_suggestions(), Duration::from_secs(300));
        }

        assert_eq!(cache.len(), 500, "size must be capped at the threshold");

        // Each insert past 500 evicts one oldest entry, so 100 inserts past
        // the threshold drop the 100 oldest (k_0..k_99).
        for i in 0..100 {
            let key = CacheKey::new("spec", &["cmd", &format!("k_{i}")], None);
            assert!(
                cache.get(&key).is_none(),
                "oldest entry k_{i} should be evicted"
            );
        }
        // Newest entries from the second batch must all survive.
        for i in 500..600 {
            let key = CacheKey::new("spec", &["cmd", &format!("k_{i}")], None);
            assert!(
                cache.get(&key).is_some(),
                "newest entry k_{i} should remain"
            );
        }
    }

    #[test]
    fn test_cache_script_template_different_prev_token_produces_different_keys() {
        let key1 = CacheKey::new(
            "test",
            &["cmd", "--flag", "value_a"],
            Some(Path::new("/tmp")),
        );
        let key2 = CacheKey::new(
            "test",
            &["cmd", "--flag", "value_b"],
            Some(Path::new("/tmp")),
        );
        assert_ne!(
            key1, key2,
            "different resolved argv should produce different cache keys"
        );
    }
}
