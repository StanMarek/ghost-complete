use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::types::Suggestion;
use tokio::sync::broadcast;

/// Cache key for generator results.
///
/// Two variants gate two distinct cache layers:
///
/// - [`CacheKey::Stdout`] keys raw script stdout. Two specs that share the
///   same `script` argv (e.g. both running `kubectl get pods`) share a stdout
///   cache entry — the spawn cost amortises across them.
/// - [`CacheKey::JsProcessed`] keys post-processed [`Suggestion`] vectors,
///   namespaced by a hash of the JS source. Two generators that share the
///   same script but ship different `js_runtime.source` bodies can never
///   cross-contaminate, because the source hash partitions their slots.
///
/// All variants compose `(spec_name, resolved_argv, cwd_if_cache_by_directory)`.
/// The `argv` field is the FULLY RESOLVED command (post-substitution for
/// `script_template`).
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum CacheKey {
    /// Raw script stdout. Shared by every consumer of the same argv —
    /// declarative-transform generators, JS-post-process generators, etc.
    Stdout {
        spec_name: String,
        resolved_argv: Vec<String>,
        cwd: Option<String>,
    },
    /// Post-processed suggestion list, partitioned by `source_hash` so two
    /// different `js_runtime.source` bodies on the same stdout can't share
    /// cached output. `source_hash` is a stable, internal-use-only digest of
    /// the JS source — not a cryptographic hash; a collision here only means
    /// two unrelated JS sources share the same suggestions, which is benign
    /// at the cache layer.
    JsProcessed {
        spec_name: String,
        resolved_argv: Vec<String>,
        cwd: Option<String>,
        source_hash: u64,
    },
    /// Typed AWS SDK provider result. `params_hash` is produced from a stable
    /// ordered representation of the provider params; profile and region are
    /// included so account/region-scoped resources cannot cross-contaminate.
    AwsSdk {
        service: String,
        operation: String,
        params_hash: u64,
        profile: Option<String>,
        region: Option<String>,
    },
}

impl CacheKey {
    /// Convenience constructor for the legacy single-key `Stdout` shape.
    pub fn new(spec_name: &str, argv: &[&str], cwd: Option<&Path>) -> Self {
        Self::Stdout {
            spec_name: spec_name.into(),
            resolved_argv: argv.iter().map(|s| s.to_string()).collect(),
            cwd: cwd.map(|p| p.to_string_lossy().to_string()),
        }
    }

    /// Like [`CacheKey::new`], but accepts an owned `&[String]` to avoid an
    /// extra allocation pass when the caller already has owned strings.
    pub fn from_strings(spec_name: &str, argv: &[String], cwd: Option<&Path>) -> Self {
        Self::Stdout {
            spec_name: spec_name.into(),
            resolved_argv: argv.to_vec(),
            cwd: cwd.map(|p| p.to_string_lossy().to_string()),
        }
    }

    /// Build a key for the post-processed cache layer. `source_hash` should
    /// be derived from the JS source via [`hash_js_source`].
    pub fn js_processed(
        spec_name: &str,
        argv: &[String],
        cwd: Option<&Path>,
        source_hash: u64,
    ) -> Self {
        Self::JsProcessed {
            spec_name: spec_name.into(),
            resolved_argv: argv.to_vec(),
            cwd: cwd.map(|p| p.to_string_lossy().to_string()),
            source_hash,
        }
    }
}

/// Stable, non-cryptographic hash of a JS source string. The result is only
/// used as a cache key partition; collisions are not a security risk because
/// every cached entry was produced from a script whose stdout the runtime
/// already trusts. Standard library `DefaultHasher` is sufficient (and
/// deterministic per process; that is all the cache layer needs).
pub fn hash_js_source(source: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

/// One stored cache entry. The `Variant` tag selects which payload variant is
/// live so the same hash map can hold both stdout strings and post-processed
/// suggestion vectors without burning two `HashMap`s + two `Mutex`es of memory
/// at idle.
struct CacheEntry {
    payload: CachedPayload,
    expires_at: Instant,
    last_accessed: Instant,
}

enum CachedPayload {
    Stdout(String),
    Suggestions(Vec<Suggestion>),
}

impl CacheEntry {
    fn is_expired(&self, now: Instant) -> bool {
        now >= self.expires_at
    }
}

/// Threshold above which `insert()` triggers an eviction sweep. Script-template
/// generator keys embed user input (e.g. `git log --author={current_token}`),
/// so an interactive typing session can manufacture unbounded distinct keys.
const CACHE_SWEEP_THRESHOLD: usize = 500;

/// In-memory TTL cache for generator results.
///
/// Thread-safe via internal `Mutex`. Entries expire after their individual TTL.
/// The same map stores both raw stdout (`CacheKey::Stdout`) and post-processed
/// suggestion vectors (`CacheKey::JsProcessed`); they cannot collide because
/// their key variants are distinct, and the variant tag flows into both
/// `Hash` and `PartialEq`.
pub struct GeneratorCache {
    entries: Mutex<HashMap<CacheKey, CacheEntry>>,
    pending: Mutex<HashMap<CacheKey, broadcast::Sender<()>>>,
}

impl GeneratorCache {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the cached suggestion vector if the key is present (under
    /// either key variant) and the payload is the suggestion shape. Stdout
    /// payloads stored via [`Self::insert_stdout`] are skipped — see
    /// [`Self::get_stdout`] for that read path.
    pub fn get(&self, key: &CacheKey) -> Option<Vec<Suggestion>> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        match entries.get_mut(key) {
            Some(entry) if !entry.is_expired(now) => {
                entry.last_accessed = now;
                match &entry.payload {
                    CachedPayload::Suggestions(s) => Some(s.clone()),
                    CachedPayload::Stdout(_) => None,
                }
            }
            Some(_) => {
                entries.remove(key);
                None
            }
            None => None,
        }
    }

    /// Insert a suggestion-vector payload. Suggestion vectors keyed by
    /// `CacheKey::Stdout` come from non-JS generators; `CacheKey::JsProcessed`
    /// holds JS post-processed output.
    pub fn insert(&self, key: CacheKey, suggestions: Vec<Suggestion>, ttl: Duration) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        entries.insert(
            key,
            CacheEntry {
                payload: CachedPayload::Suggestions(suggestions),
                expires_at: now + ttl,
                last_accessed: now,
            },
        );
        Self::sweep_if_oversized(&mut entries, now);
    }

    /// Return a cached suggestion vector or run one async producer for this
    /// key while concurrent callers wait for the result to be inserted.
    ///
    /// The entries lock is only held for synchronous cache probes/inserts; the
    /// producer future is awaited without holding any cache mutex.
    pub async fn get_or_try_insert_with<E, F, Fut>(
        &self,
        key: CacheKey,
        ttl: Duration,
        producer: F,
    ) -> Result<Vec<Suggestion>, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Vec<Suggestion>, E>>,
    {
        loop {
            if let Some(suggestions) = self.get(&key) {
                return Ok(suggestions);
            }

            enum PendingProbe {
                Wait(broadcast::Receiver<()>),
                Lead(broadcast::Sender<()>),
            }

            let probe = {
                let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(sender) = pending.get(&key) {
                    PendingProbe::Wait(sender.subscribe())
                } else {
                    let (sender, _) = broadcast::channel(1);
                    pending.insert(key.clone(), sender.clone());
                    PendingProbe::Lead(sender)
                }
            };

            let sender = match probe {
                PendingProbe::Wait(mut receiver) => {
                    let _ = receiver.recv().await;
                    continue;
                }
                PendingProbe::Lead(sender) => sender,
            };

            let result = producer().await;
            if let Ok(suggestions) = &result {
                self.insert(key.clone(), suggestions.clone(), ttl);
            }

            {
                let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
                pending.remove(&key);
            }
            let _ = sender.send(());
            return result;
        }
    }

    /// Look up a cached raw-stdout entry. Returns `None` if absent, expired,
    /// or stored under a different payload shape.
    pub fn get_stdout(&self, key: &CacheKey) -> Option<String> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        match entries.get_mut(key) {
            Some(entry) if !entry.is_expired(now) => {
                entry.last_accessed = now;
                match &entry.payload {
                    CachedPayload::Stdout(s) => Some(s.clone()),
                    CachedPayload::Suggestions(_) => None,
                }
            }
            Some(_) => {
                entries.remove(key);
                None
            }
            None => None,
        }
    }

    /// Insert a raw-stdout payload so two different JS post-processors
    /// operating on the same script can share the spawn cost.
    pub fn insert_stdout(&self, key: CacheKey, stdout: String, ttl: Duration) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        entries.insert(
            key,
            CacheEntry {
                payload: CachedPayload::Stdout(stdout),
                expires_at: now + ttl,
                last_accessed: now,
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
        // Pass 2: still oversize — drop the least recently used entries
        // until we're back at the threshold.
        let excess = entries.len() - CACHE_SWEEP_THRESHOLD;
        let mut by_access: Vec<(Instant, CacheKey)> = entries
            .iter()
            .map(|(k, v)| (v.last_accessed, k.clone()))
            .collect();
        by_access.sort_by_key(|(t, _)| *t);
        for (_, key) in by_access.into_iter().take(excess) {
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
        // 600 entries with future TTLs — none are expired, so the sweep
        // evicts the least recently accessed entries. End state: exactly
        // the 500 entries with the most recent access time.
        let cache = GeneratorCache::new();

        for i in 0..500 {
            let key = CacheKey::new("spec", &["cmd", &format!("k_{i}")], None);
            cache.insert(key, make_suggestions(), Duration::from_secs(300));
        }
        // Force a clear access-time gap so the LRU drop is deterministic:
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
    fn test_cache_lru_access_prevents_eviction() {
        // Verify that accessing a cache entry updates its LRU position.
        // Insert 500 entries, access the first 100, then insert 100 more
        // to trigger a sweep. The accessed entries should survive; the
        // non-accessed old entries should be evicted.
        let cache = GeneratorCache::new();

        for i in 0..500 {
            let key = CacheKey::new("spec", &["cmd", &format!("k_{i}")], None);
            cache.insert(key, make_suggestions(), Duration::from_secs(300));
        }

        std::thread::sleep(Duration::from_millis(2));

        // Access entries 0-99 to refresh their LRU timestamp
        for i in 0..100 {
            let key = CacheKey::new("spec", &["cmd", &format!("k_{i}")], None);
            assert!(cache.get(&key).is_some());
        }

        std::thread::sleep(Duration::from_millis(2));

        // Insert 100 more to push past the threshold
        for i in 500..600 {
            let key = CacheKey::new("spec", &["cmd", &format!("k_{i}")], None);
            cache.insert(key, make_suggestions(), Duration::from_secs(300));
        }

        assert_eq!(cache.len(), 500);

        // The accessed entries (0-99) should survive
        for i in 0..100 {
            let key = CacheKey::new("spec", &["cmd", &format!("k_{i}")], None);
            assert!(
                cache.get(&key).is_some(),
                "recently accessed k_{i} should survive LRU eviction"
            );
        }

        // The non-accessed old entries (100-199) should be evicted first
        for i in 100..200 {
            let key = CacheKey::new("spec", &["cmd", &format!("k_{i}")], None);
            assert!(
                cache.get(&key).is_none(),
                "non-accessed k_{i} should be evicted"
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

    #[test]
    fn test_stdout_and_js_processed_are_distinct_keyspaces() {
        // Same spec/argv/cwd: a `Stdout` lookup must NEVER hit a
        // `JsProcessed` slot, and vice versa. The variant tag flows into
        // both `Hash` and `PartialEq`, so they live in independent slots
        // even though every other field is identical.
        let stdout_key = CacheKey::Stdout {
            spec_name: "spec".into(),
            resolved_argv: vec!["cmd".into()],
            cwd: None,
        };
        let js_key = CacheKey::JsProcessed {
            spec_name: "spec".into(),
            resolved_argv: vec!["cmd".into()],
            cwd: None,
            source_hash: 0,
        };
        assert_ne!(stdout_key, js_key);
    }

    #[test]
    fn test_two_js_sources_produce_distinct_cache_keys() {
        let argv = vec!["kubectl".to_string(), "get".to_string(), "pods".to_string()];
        let key_a = CacheKey::js_processed(
            "kubectl",
            &argv,
            Some(Path::new("/repo")),
            hash_js_source("out => out.split('\\n').map(name => ({ name }))"),
        );
        let key_b = CacheKey::js_processed(
            "kubectl",
            &argv,
            Some(Path::new("/repo")),
            hash_js_source("out => out.split('\\n').map(n => ({ name: n.toUpperCase() }))"),
        );
        assert_ne!(
            key_a, key_b,
            "two different js_runtime.source bodies must hash to distinct cache keys"
        );
    }

    #[test]
    fn test_stdout_payload_is_not_returned_via_get() {
        // `insert_stdout` keys raw strings; `get()` must skip those slots
        // so a stdout value never bleeds into the suggestion-shaped API.
        let cache = GeneratorCache::new();
        let key = CacheKey::Stdout {
            spec_name: "kubectl".into(),
            resolved_argv: vec!["kubectl".into(), "get".into(), "pods".into()],
            cwd: None,
        };
        cache.insert_stdout(key.clone(), "raw\nstdout".into(), Duration::from_secs(60));
        assert!(cache.get(&key).is_none());
        assert_eq!(cache.get_stdout(&key).as_deref(), Some("raw\nstdout"));
    }

    #[test]
    fn test_suggestion_payload_is_not_returned_via_get_stdout() {
        let cache = GeneratorCache::new();
        let key = CacheKey::Stdout {
            spec_name: "kubectl".into(),
            resolved_argv: vec!["kubectl".into(), "get".into(), "pods".into()],
            cwd: None,
        };
        cache.insert(key.clone(), make_suggestions(), Duration::from_secs(60));
        assert!(cache.get_stdout(&key).is_none());
        assert!(cache.get(&key).is_some());
    }
}
