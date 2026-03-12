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
}

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
    pub fn get(&self, key: &CacheKey) -> Option<Vec<Suggestion>> {
        let entries = self.entries.lock().unwrap();
        entries.get(key).and_then(|entry| {
            if Instant::now() < entry.expires_at {
                Some(entry.suggestions.clone())
            } else {
                None
            }
        })
    }

    /// Insert (or replace) a cache entry with the given TTL.
    pub fn insert(&self, key: CacheKey, suggestions: Vec<Suggestion>, ttl: Duration) {
        let mut entries = self.entries.lock().unwrap();
        entries.insert(
            key,
            CacheEntry {
                suggestions,
                expires_at: Instant::now() + ttl,
            },
        );
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
