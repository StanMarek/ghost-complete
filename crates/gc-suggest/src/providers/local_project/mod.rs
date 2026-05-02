//! Local-project providers — file-driven completion sources that parse
//! a project's local manifest (`Makefile`, `package.json`, `Cargo.toml`)
//! to produce suggestions. They replace `requires_js: true` Fig
//! generators that would otherwise have produced an empty popup for
//! `make <TAB>`, `npm run <TAB>`, `cargo run -p <TAB>`, etc.
//!
//! ### Pattern
//!
//! `MakefileTargets` and `NpmScripts` follow the shared `MtimeCache`
//! shape:
//! 1. Walk up to 32 ancestors of `ctx.cwd` to find the relevant file.
//! 2. Look the path up in a module-private [`MtimeCache`].
//! 3. On miss: read bytes, parse, store, return.
//! 4. On hit: return the cached value.
//!
//! `CargoWorkspaceMembers` also walks ancestors, but uses its own
//! `CargoCache`: cache hits require every recorded per-path stamp
//! (root/member manifests, glob-prefix dirs, and missing-path probes)
//! to still match the live filesystem.
//!
//! No subprocesses, no filesystem watchers. The caches are
//! process-local; a cold `ghost-complete` restart starts empty. See
//! `docs/PROVIDERS.md` §Local-project providers for the rationale.

pub mod cargo_workspace;
pub mod makefile;
pub mod npm_scripts;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

/// Hard cap on cached entries per `MtimeCache`-backed provider. These
/// files are tiny (10–200 KB parsed), and cargo's separate `CargoCache`
/// has the same 64-entry cap. The cap exists to keep memory bounded if
/// a user `cd`s through many distinct projects in a single session.
const MAX_CACHE_ENTRIES: usize = 64;

/// Maximum ancestor levels to walk when discovering a project file.
/// Defuses pathological symlink loops without limiting realistic repo
/// depth.
pub(crate) const MAX_ANCESTOR_WALK: usize = 32;

#[derive(Clone)]
struct CacheEntry<T> {
    /// `None` means `metadata.modified()` failed (rare — some FUSE/
    /// network mounts). Equality of `None == None` must NOT count as
    /// a hit; the `get_or_insert_with` probe short-circuits on `None`.
    mtime: Option<SystemTime>,
    size: u64,
    /// Insertion sequence — used for FIFO eviction at capacity. Cache
    /// hits do not refresh this value (true LRU would require a write
    /// lock on every read).
    inserted_at_seq: u64,
    value: T,
}

/// Cache keyed by absolute file path with `(mtime, size)` invalidation.
/// Each `MtimeCache`-backed provider owns one `MtimeCache<T>` where
/// `T` is the parsed shape it cares about (e.g. `Vec<String>` for
/// makefile targets).
///
/// On every `get_or_insert_with`, the file's `metadata` is read first.
/// If `(mtime, size)` matches the cached entry, the cached value is
/// cloned and returned without re-reading the file. Otherwise the
/// extractor is called against fresh bytes and the result replaces the
/// stale entry.
///
/// FIFO eviction: when a new key is inserted and the cache is at
/// capacity, the entry with the lowest `inserted_at_seq` is dropped.
/// This is O(N) over a 64-entry cap — fine, since insertion only
/// happens on cold reads (which are already paying file IO cost).
pub(crate) struct MtimeCache<T: Clone> {
    inner: Mutex<MtimeCacheInner<T>>,
}

struct MtimeCacheInner<T> {
    entries: HashMap<PathBuf, CacheEntry<T>>,
    next_seq: u64,
}

impl<T: Clone> MtimeCache<T> {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(MtimeCacheInner {
                entries: HashMap::new(),
                next_seq: 0,
            }),
        }
    }

    /// Return the cached value for `path` if `(mtime, size)` are
    /// unchanged, otherwise read the file and call `extractor` to
    /// produce a fresh value, store it, and return it.
    ///
    /// `extractor` returns the parsed value directly. To indicate a
    /// parse failure, return whatever empty/default value makes sense
    /// for `T` — providers translate empty parses into empty
    /// suggestion vecs anyway.
    ///
    /// Returns `None` if the file's metadata can't be read or its
    /// bytes can't be loaded. The caller logs and returns an empty
    /// suggestion vec on `None` — the same contract every other
    /// provider's failure path uses.
    pub(crate) fn get_or_insert_with<F>(&self, path: &Path, extractor: F) -> Option<T>
    where
        F: FnOnce(&[u8]) -> T,
    {
        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "local-project provider: metadata read failed"
                );
                return None;
            }
        };
        let mtime = match metadata.modified() {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "local-project provider: file metadata has no mtime; cache validity downgraded — re-reading on every call"
                );
                None
            }
        };
        let size = metadata.len();

        // `mtime == None` short-circuits the cache: we never compare
        // None == None as a hit, so the extractor always runs and the
        // value is returned without storing a stale-prone entry.
        if let Some(probe_mtime) = mtime {
            let guard = match self.inner.lock() {
                Ok(g) => g,
                Err(poisoned) => {
                    tracing::error!(
                        path = %path.display(),
                        "local-project provider: mtime cache mutex poisoned (read path); recovering"
                    );
                    poisoned.into_inner()
                }
            };
            if let Some(entry) = guard.entries.get(path) {
                if entry.mtime == Some(probe_mtime) && entry.size == size {
                    return Some(entry.value.clone());
                }
            }
            drop(guard);
        }

        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "local-project provider: file read failed"
                );
                return None;
            }
        };
        let value = extractor(&bytes);

        if mtime.is_none() {
            return Some(value);
        }

        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                tracing::error!(
                    path = %path.display(),
                    "local-project provider: mtime cache mutex poisoned (write path); recovering"
                );
                poisoned.into_inner()
            }
        };
        let inserted_at_seq = guard.next_seq;
        guard.next_seq = guard.next_seq.wrapping_add(1);

        if !guard.entries.contains_key(path) && guard.entries.len() >= MAX_CACHE_ENTRIES {
            if let Some(victim) = guard
                .entries
                .iter()
                .min_by_key(|(_, e)| e.inserted_at_seq)
                .map(|(p, _)| p.clone())
            {
                guard.entries.remove(&victim);
            }
        }

        guard.entries.insert(
            path.to_path_buf(),
            CacheEntry {
                mtime,
                size,
                inserted_at_seq,
                value: value.clone(),
            },
        );

        Some(value)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        match self.inner.lock() {
            Ok(g) => g.entries.len(),
            Err(poisoned) => poisoned.into_inner().entries.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_file(dir: &Path, name: &str, body: &[u8]) -> PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body).unwrap();
        f.sync_all().unwrap();
        path
    }

    #[test]
    fn cache_hit_returns_cached_value_without_reextracting() {
        let tmp = TempDir::new().unwrap();
        let path = write_file(tmp.path(), "data.txt", b"hello");
        let cache: MtimeCache<usize> = MtimeCache::new();

        let count = std::sync::Mutex::new(0usize);
        let extractor = |b: &[u8]| {
            let mut g = count.lock().unwrap();
            *g += 1;
            b.len()
        };

        let v1 = cache.get_or_insert_with(&path, extractor).unwrap();
        let v2 = cache.get_or_insert_with(&path, extractor).unwrap();
        assert_eq!(v1, 5);
        assert_eq!(v2, 5);
        assert_eq!(*count.lock().unwrap(), 1, "extractor must run only once");
    }

    #[test]
    fn mtime_change_invalidates() {
        let tmp = TempDir::new().unwrap();
        let path = write_file(tmp.path(), "data.txt", b"hello");
        let cache: MtimeCache<usize> = MtimeCache::new();

        let runs = std::sync::Mutex::new(0usize);
        let extractor = |b: &[u8]| {
            *runs.lock().unwrap() += 1;
            b.len()
        };

        let _ = cache.get_or_insert_with(&path, extractor).unwrap();
        // Bump mtime forward without changing size by setting a future mtime.
        let future = std::time::SystemTime::now() + std::time::Duration::from_secs(120);
        let ft = filetime::FileTime::from_system_time(future);
        filetime::set_file_mtime(&path, ft).unwrap();
        let _ = cache.get_or_insert_with(&path, extractor).unwrap();

        assert_eq!(
            *runs.lock().unwrap(),
            2,
            "mtime change must invalidate cache"
        );
    }

    #[test]
    fn size_change_invalidates() {
        let tmp = TempDir::new().unwrap();
        let path = write_file(tmp.path(), "data.txt", b"hello");
        let cache: MtimeCache<usize> = MtimeCache::new();

        let runs = std::sync::Mutex::new(0usize);
        let extractor = |b: &[u8]| {
            *runs.lock().unwrap() += 1;
            b.len()
        };

        let _ = cache.get_or_insert_with(&path, extractor).unwrap();

        // Rewrite with different content of different length, then pin the
        // mtime back to what it was so we exercise the size-change branch
        // in isolation.
        let original_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"hello world!").unwrap();
        f.sync_all().unwrap();
        let ft = filetime::FileTime::from_system_time(original_mtime);
        filetime::set_file_mtime(&path, ft).unwrap();

        let _ = cache.get_or_insert_with(&path, extractor).unwrap();
        assert_eq!(*runs.lock().unwrap(), 2, "size change must invalidate");
    }

    #[test]
    fn lru_evicts_oldest_at_capacity() {
        let tmp = TempDir::new().unwrap();
        let cache: MtimeCache<usize> = MtimeCache::new();

        let mut paths = Vec::new();
        for i in 0..MAX_CACHE_ENTRIES {
            let p = write_file(tmp.path(), &format!("f{i}.txt"), b"x");
            cache.get_or_insert_with(&p, |b| b.len()).unwrap();
            paths.push(p);
        }
        assert_eq!(cache.len(), MAX_CACHE_ENTRIES);

        // Inserting a 65th key must evict exactly one entry, keeping us
        // at the cap.
        let extra = write_file(tmp.path(), "extra.txt", b"x");
        cache.get_or_insert_with(&extra, |b| b.len()).unwrap();
        assert_eq!(cache.len(), MAX_CACHE_ENTRIES);

        // The oldest entry (paths[0]) should have been evicted; touching
        // it again must trigger a fresh extraction.
        let runs = std::sync::Mutex::new(0usize);
        let extractor = |b: &[u8]| {
            *runs.lock().unwrap() += 1;
            b.len()
        };
        cache.get_or_insert_with(&paths[0], extractor).unwrap();
        assert_eq!(*runs.lock().unwrap(), 1, "oldest must have been evicted");
    }

    #[test]
    fn missing_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        let cache: MtimeCache<usize> = MtimeCache::new();
        let result = cache.get_or_insert_with(&tmp.path().join("nope.txt"), |b| b.len());
        assert!(result.is_none());
    }

    #[test]
    fn deletion_then_recreate_drops_stale_value() {
        // Cache hit, then file removed: next call returns None
        // (metadata read fails). Recreating with new bytes must trigger
        // the extractor and surface the new value, not the stale one.
        let tmp = TempDir::new().unwrap();
        let path = write_file(tmp.path(), "data.txt", b"hello");
        let cache: MtimeCache<usize> = MtimeCache::new();

        let runs = std::sync::Mutex::new(0usize);
        let extractor = |b: &[u8]| {
            *runs.lock().unwrap() += 1;
            b.len()
        };

        let v1 = cache.get_or_insert_with(&path, extractor).unwrap();
        assert_eq!(v1, 5);

        std::fs::remove_file(&path).unwrap();
        let after_delete = cache.get_or_insert_with(&path, extractor);
        assert!(after_delete.is_none(), "deletion must surface as None");

        let path = write_file(tmp.path(), "data.txt", b"hello world!");
        let v2 = cache.get_or_insert_with(&path, extractor).unwrap();
        assert_eq!(v2, 12, "must extract fresh value, not return stale 5");
        assert_eq!(
            *runs.lock().unwrap(),
            2,
            "extractor must run for the original write and the recreated file"
        );
    }

    #[test]
    fn fifo_eviction_does_not_promote_on_hit() {
        // Cache is FIFO-on-insert, not true LRU: hitting an entry in
        // the middle of the cache does NOT refresh its position, so it
        // is still the oldest by inserted_at_seq and gets evicted when
        // a new key arrives at capacity.
        let tmp = TempDir::new().unwrap();
        let cache: MtimeCache<usize> = MtimeCache::new();

        let mut paths = Vec::new();
        for i in 0..MAX_CACHE_ENTRIES {
            let p = write_file(tmp.path(), &format!("f{i}.txt"), b"x");
            cache.get_or_insert_with(&p, |b| b.len()).unwrap();
            paths.push(p);
        }
        assert_eq!(cache.len(), MAX_CACHE_ENTRIES);

        // Touch the oldest entry — a hit, no re-insert.
        cache.get_or_insert_with(&paths[0], |b| b.len()).unwrap();

        // Insert a new key. Under true LRU paths[0] would survive;
        // under FIFO it is the victim.
        let extra = write_file(tmp.path(), "extra.txt", b"x");
        cache.get_or_insert_with(&extra, |b| b.len()).unwrap();

        let runs = std::sync::Mutex::new(0usize);
        let extractor = |b: &[u8]| {
            *runs.lock().unwrap() += 1;
            b.len()
        };
        cache.get_or_insert_with(&paths[0], extractor).unwrap();
        assert_eq!(
            *runs.lock().unwrap(),
            1,
            "FIFO: a hit does not promote, so paths[0] must still have been evicted"
        );
    }
}
