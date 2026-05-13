//! Embedded completion specs.
//!
//! ## Why this exists
//!
//! `ghost-complete` ships with 709 Fig-compatible completion specs baked
//! into the binary. The embedded payload is produced by the crate's
//! `build.rs`, which reads every `specs/*.json`, strips the
//! runtime-unused `js_source` field from generators, re-serialises each
//! spec compactly (no pretty-print whitespace), zstd-compresses each spec
//! body at level 19, and packs the result into a single binary archive
//! at `$OUT_DIR/embedded_specs.bin`. The runtime `include_bytes!`s that
//! archive and parses it once into an in-memory index on first lookup.
//!
//! Originally these embedded specs only existed to be copied to disk by
//! `ghost-complete install`. That left a latent bug: a user who ran
//! `cargo install ghost-complete` and then launched `ghost-complete`
//! (without first running `install`) loaded zero specs and got no error
//! — autocomplete silently degraded to filesystem + history + `$PATH`
//! only.
//!
//! ## Runtime path (v0.15+)
//!
//! The embedded specs are now consumed in-memory by
//! [`crate::specs::SpecStore::load_with_embedded`] via
//! [`embedded_filenames_with_aliases`]: the spec loader registers each
//! `(filename, name_alias)` pair as a lazy
//! [`crate::specs::SpecSource::Embedded`] entry, and the JSON body is
//! pulled on first parse via [`embedded_spec_contents`] which returns a
//! `&'static str` slice cached after first decompression. No disk
//! materialisation, no `.cache` write on first run, no version sentinel.
//!
//! Filenames and aliases live in the parsed-once index uncompressed;
//! only spec bodies are zstd-compressed. First lookup decompresses the
//! body, then `Box::leak`s the resulting `String` into `&'static str`
//! and caches the pointer so subsequent lookups are O(1) and pointer-stable.
//!
//! Earlier versions (≤ v0.12.3) used [`embedded_cache_dir`] as a
//! materialisation target; [`purge_embedded_cache_if_present`] removes
//! that orphaned directory so upgraders don't keep a 25 MB stale copy
//! around indefinitely. `ghost-complete install` and `ghost-complete
//! uninstall` both invoke it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Compressed-archive bytes embedded by `build.rs`. Format documented at
/// the top of `build.rs`.
const ARCHIVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/embedded_specs.bin"));

/// Parsed-once header + per-filename blob slice lookup + lazy
/// decompression cache.
///
/// `filenames` and `aliases` are index-aligned with the entry order
/// emitted by `build.rs` (sorted by filename). `blobs` is a flat
/// filename → compressed-slice map. `cache` is filled on demand by
/// [`embedded_spec_contents`] — decompressed JSON is `Box::leak`ed into
/// a `&'static str` and stored, so subsequent lookups for the same
/// filename return the identical pointer.
struct EmbeddedIndex {
    filenames: Vec<&'static str>,
    aliases: Vec<Option<&'static str>>,
    blobs: HashMap<&'static str, &'static [u8]>,
    cache: Mutex<HashMap<&'static str, &'static str>>,
}

/// One-shot initialiser. Parses [`ARCHIVE`] the first time any public
/// entry point touches the index.
static INDEX: OnceLock<EmbeddedIndex> = OnceLock::new();

fn index() -> &'static EmbeddedIndex {
    INDEX.get_or_init(parse_archive)
}

/// Walk the [`ARCHIVE`] header once, recording every entry's filename,
/// optional alias, and compressed blob slice. Slice bounds are validated
/// — a truncated archive panics with a clear message because the only
/// way the archive can be truncated is a build-script bug, and a silent
/// load-zero-specs outcome would be much worse than a loud crash.
fn parse_archive() -> EmbeddedIndex {
    let mut cursor: usize = 0;
    let entry_count = read_u32(ARCHIVE, &mut cursor) as usize;

    // Minimum per-entry footprint = [u16 name_len][>=1 name byte][u8 has_alias=0][u32 blob_len][>=0 blob]
    // = 2 + 1 + 1 + 4 = 8 bytes. A truncated or otherwise-corrupt
    // entry_count larger than (ARCHIVE.len() - 4) / 8 cannot
    // possibly be valid, and an unsanitised value would request a
    // multi-gigabyte allocation in `with_capacity` below. Convert
    // that into a clear diagnostic instead of an OOM crash.
    const MIN_ENTRY_BYTES: usize = 8;
    let body_bytes = ARCHIVE.len().saturating_sub(4);
    let max_plausible = body_bytes / MIN_ENTRY_BYTES;
    assert!(
        entry_count <= max_plausible,
        "embedded_specs.bin: entry_count {entry_count} is implausibly large for archive of {} bytes \
         (max plausible {max_plausible}) — archive is truncated or has a corrupt header",
        ARCHIVE.len()
    );

    let mut filenames: Vec<&'static str> = Vec::with_capacity(entry_count);
    let mut aliases: Vec<Option<&'static str>> = Vec::with_capacity(entry_count);
    let mut blobs: HashMap<&'static str, &'static [u8]> = HashMap::with_capacity(entry_count);

    for _ in 0..entry_count {
        let filename = read_short_string(ARCHIVE, &mut cursor);
        let has_alias = read_u8(ARCHIVE, &mut cursor);
        let alias = match has_alias {
            0 => None,
            1 => Some(read_short_string(ARCHIVE, &mut cursor)),
            other => panic!(
                "embedded_specs.bin: invalid has_alias byte {other} at offset {cursor} \
                 (build-script bug — only 0 or 1 should appear)"
            ),
        };
        let blob_len = read_u32(ARCHIVE, &mut cursor) as usize;
        let blob_end = cursor
            .checked_add(blob_len)
            .expect("embedded_specs.bin: blob length overflows usize");
        if blob_end > ARCHIVE.len() {
            panic!(
                "embedded_specs.bin: blob at offset {cursor} (len {blob_len}) extends past archive end ({}) — \
                 archive is truncated",
                ARCHIVE.len()
            );
        }
        let blob: &'static [u8] = &ARCHIVE[cursor..blob_end];
        cursor = blob_end;

        filenames.push(filename);
        aliases.push(alias);
        let prev = blobs.insert(filename, blob);
        debug_assert!(
            prev.is_none(),
            "embedded_specs.bin: duplicate filename in archive: {filename} (build-script bug)"
        );
    }

    assert_eq!(
        cursor,
        ARCHIVE.len(),
        "embedded_specs.bin: trailing bytes after parsing {entry_count} entries"
    );

    EmbeddedIndex {
        filenames,
        aliases,
        blobs,
        cache: Mutex::new(HashMap::new()),
    }
}

/// Read a little-endian `u32` from `bytes` at `*cursor`, advance `cursor`
/// by 4. Panics on out-of-bounds (build-script bug — see
/// [`parse_archive`]).
fn read_u32(bytes: &[u8], cursor: &mut usize) -> u32 {
    let end = cursor
        .checked_add(4)
        .expect("embedded_specs.bin: cursor overflow reading u32");
    if end > bytes.len() {
        panic!(
            "embedded_specs.bin: out-of-bounds u32 read at offset {} (archive len {})",
            *cursor,
            bytes.len()
        );
    }
    let slice = &bytes[*cursor..end];
    *cursor = end;
    u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]])
}

/// Read a single byte from `bytes` at `*cursor`, advance `cursor` by 1.
fn read_u8(bytes: &[u8], cursor: &mut usize) -> u8 {
    if *cursor >= bytes.len() {
        panic!(
            "embedded_specs.bin: out-of-bounds u8 read at offset {} (archive len {})",
            *cursor,
            bytes.len()
        );
    }
    let b = bytes[*cursor];
    *cursor += 1;
    b
}

/// Read a `[u16 len][utf-8 bytes]` short-string from `bytes` at `*cursor`,
/// advance `cursor` past the payload, return a `&'static str` slice into
/// `bytes`. Panics on out-of-bounds or invalid UTF-8 (build-script bug —
/// every filename and alias was a `&str` in `build.rs`).
fn read_short_string(bytes: &'static [u8], cursor: &mut usize) -> &'static str {
    let len_end = cursor
        .checked_add(2)
        .expect("embedded_specs.bin: cursor overflow reading short-string len");
    if len_end > bytes.len() {
        panic!(
            "embedded_specs.bin: out-of-bounds u16 read at offset {} (archive len {})",
            *cursor,
            bytes.len()
        );
    }
    let len = u16::from_le_bytes([bytes[*cursor], bytes[*cursor + 1]]) as usize;
    let str_start = len_end;
    let str_end = str_start
        .checked_add(len)
        .expect("embedded_specs.bin: cursor overflow reading short-string body");
    if str_end > bytes.len() {
        panic!(
            "embedded_specs.bin: short-string at offset {str_start} (len {len}) extends past archive end ({}) — \
             archive is truncated",
            bytes.len()
        );
    }
    let slice = &bytes[str_start..str_end];
    *cursor = str_end;
    std::str::from_utf8(slice).unwrap_or_else(|e| {
        panic!(
            "embedded_specs.bin: short-string at offset {str_start} is not valid UTF-8: {e} \
             (build-script bug)"
        )
    })
}

/// Number of embedded specs. Cheap — reads the parsed-once header.
pub fn embedded_spec_count() -> usize {
    index().filenames.len()
}

/// Every embedded filename in archive emit order (sorted ascending by
/// `build.rs`). Cheap — borrows the parsed-once header.
pub fn embedded_filenames() -> &'static [&'static str] {
    &index().filenames
}

/// Look up an embedded spec's JSON contents by filename (e.g. `"git.json"`).
///
/// Returns `Some(contents)` when the binary ships with that spec.
///
/// Lookup is O(1) on both cache hit and cache miss: the blob map keys
/// are the archive-owned `&'static str` filenames, so a single
/// [`HashMap::get_key_value`] yields both the cache key and the
/// compressed blob without a linear scan.
///
/// The first call for a given filename zstd-decompresses the blob from
/// the archive and `Box::leak`s the resulting `String` into a `&'static
/// str` that is cached for the process lifetime. Subsequent lookups
/// return the identical pointer — callers can rely on pointer-stability
/// for `Cow::Borrowed` round-trips. Net memory cost across the corpus
/// (~47 MB minified JSON) is bounded by the working set; specs that are
/// never queried never pay the decompression cost.
pub fn embedded_spec_contents(filename: &str) -> Option<&'static str> {
    let idx = index();

    // The cache is keyed by the `&'static str` filename in the archive,
    // not the caller's `&str`. `get_key_value` hands us both in one
    // O(1) hash lookup — no linear scan over `idx.filenames`.
    let (archive_key, blob) = idx.blobs.get_key_value(filename).map(|(k, v)| (*k, *v))?;

    // Fast path: cache hit.
    {
        let guard = idx.cache.lock().unwrap_or_else(|poison| {
            tracing::error!(
                "embedded spec cache mutex was poisoned by an earlier panic; \
                 recovering the inner map (suggestions may be stale until process restart)"
            );
            poison.into_inner()
        });
        if let Some(s) = guard.get(archive_key) {
            return Some(*s);
        }
    }

    // Slow path: decompress, leak, install. The lock is dropped during
    // decompression to keep concurrent first-touches of different specs
    // parallel; a same-filename race leaks both decompressed bodies and
    // keeps one — acceptable because (a) the leak is one-time per spec
    // and (b) under load the cache fills monotonically.
    let bytes = zstd::decode_all(blob).unwrap_or_else(|e| {
        panic!(
            "embedded_specs.bin: zstd decode failed for {archive_key}: {e} (build-script or \
             archive-format bug)"
        )
    });
    let owned = String::from_utf8(bytes).unwrap_or_else(|e| {
        panic!(
            "embedded_specs.bin: spec {archive_key} decompressed to invalid UTF-8: {e} \
             (build-script bug — input was a UTF-8 str)"
        )
    });
    let leaked: &'static str = Box::leak(owned.into_boxed_str());

    let mut guard = idx.cache.lock().unwrap_or_else(|poison| {
        tracing::error!(
            "embedded spec cache mutex was poisoned by an earlier panic; \
             recovering the inner map (suggestions may be stale until process restart)"
        );
        poison.into_inner()
    });
    // Re-check: another thread may have populated the slot between our
    // lock-drop and re-acquire. If so, prefer the existing pointer so
    // callers that hold both side-by-side compare equal.
    if let Some(existing) = guard.get(archive_key) {
        return Some(*existing);
    }
    guard.insert(archive_key, leaked);
    Some(leaked)
}

/// Eager iterator that yields `(filename, contents, name_alias)` for
/// every embedded spec in emit order. The alias is the
/// `CompletionSpec.name` field captured at build time when it differs
/// from the filename stem; `None` otherwise.
///
/// **Warning: this decompresses every spec body it touches**, which
/// `Box::leak`s ~47 MB of JSON across the full corpus. The startup spec
/// loader (`SpecStore::load_with_embedded`) uses
/// [`embedded_filenames_with_aliases`] instead, which carries just the
/// filename + alias pair and lets the lazy parse path pull the body on
/// first touch. Use this triple iterator sparingly — it exists for
/// downstream test consumers that want a single iterator over the full
/// corpus.
pub fn embedded_entries_with_aliases(
) -> impl Iterator<Item = (&'static str, &'static str, Option<&'static str>)> {
    let idx = index();
    idx.filenames
        .iter()
        .copied()
        .zip(idx.aliases.iter().copied())
        .map(|(filename, alias)| {
            let contents = embedded_spec_contents(filename)
                .expect("filename listed in index must round-trip through embedded_spec_contents");
            (filename, contents, alias)
        })
}

/// Iterate every embedded `(filename, name_alias)` pair in emit order,
/// **without decompressing any spec bodies**. Used by the startup spec
/// loader to register filename-stem and name-alias entries at zero
/// decompression cost — bodies are pulled on first parse via
/// [`embedded_spec_contents`].
pub fn embedded_filenames_with_aliases(
) -> impl Iterator<Item = (&'static str, Option<&'static str>)> {
    let idx = index();
    idx.filenames
        .iter()
        .copied()
        .zip(idx.aliases.iter().copied())
}

/// Path of the legacy `~/.cache/ghost-complete/embedded-specs/` directory
/// where pre-v0.12.4 binaries materialised the embedded corpus on first
/// run. v0.12.4+ no longer writes here; the only remaining caller is
/// [`purge_embedded_cache_if_present`], which deletes the directory if a
/// previous version left it behind.
pub fn embedded_cache_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| {
        h.join(".cache")
            .join("ghost-complete")
            .join("embedded-specs")
    })
}

/// Remove the legacy embedded-spec cache directory at `dir` if it exists.
///
/// Returns `Ok(Some(path))` when a directory was deleted, `Ok(None)`
/// when nothing needed cleanup (dir absent, path is not a directory, or
/// path is a symlink which we refuse to follow), and an error when deletion
/// failed.
///
/// Symlink safety: a same-user attacker who replaces the cache dir with
/// a symlink before `ghost-complete install` runs could otherwise
/// redirect `remove_dir_all` at any directory the user can write to. We
/// detect symlinks via `symlink_metadata` and refuse to act — leaving
/// the symlink intact for the user to investigate.
pub fn purge_embedded_cache_at(dir: &Path) -> std::io::Result<Option<PathBuf>> {
    let meta = match std::fs::symlink_metadata(dir) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if meta.file_type().is_symlink() {
        tracing::warn!(
            dir = %dir.display(),
            "legacy embedded-spec cache path is a symlink; refusing to remove"
        );
        return Ok(None);
    }
    if !meta.is_dir() {
        return Ok(None);
    }
    std::fs::remove_dir_all(dir)?;
    Ok(Some(dir.to_path_buf()))
}

/// Remove the legacy embedded-spec cache directory if it exists.
///
/// Returns `Ok(Some(path))` when a directory was deleted, `Ok(None)`
/// when nothing needed cleanup (no home dir, dir absent, or path is a
/// symlink which we refuse to follow), and an error when deletion failed.
pub fn purge_embedded_cache_if_present() -> std::io::Result<Option<PathBuf>> {
    let Some(dir) = embedded_cache_dir() else {
        return Ok(None);
    };
    purge_embedded_cache_at(&dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn embedded_spec_aliases_aligns_with_filenames() {
        let idx = index();
        assert_eq!(
            idx.filenames.len(),
            idx.aliases.len(),
            "filenames and aliases must have matching lengths"
        );
        for filename in embedded_filenames() {
            assert!(
                idx.blobs.contains_key(filename),
                "filename {filename} missing from blobs map"
            );
        }
    }

    #[test]
    fn embedded_entries_with_aliases_yields_known_alias() {
        // appwrite.json declares `name: "index"` — the build.rs captures
        // it as a non-stem alias. Pin a known live entry so a future
        // refactor of the build script can't silently zero out the table.
        let appwrite = embedded_entries_with_aliases()
            .find(|(filename, _, _)| *filename == "appwrite.json")
            .expect("appwrite.json must ship in the embedded corpus");
        assert_eq!(appwrite.2, Some("index"));
    }

    #[test]
    fn embedded_spec_alias_manifest_matches_parsed_spec_names() {
        for (filename, contents, name_alias) in embedded_entries_with_aliases() {
            let stem = filename
                .strip_suffix(".json")
                .expect("embedded spec filenames end in .json");
            let parsed = crate::specs::parse_spec_checked_and_sanitized(contents)
                .unwrap_or_else(|e| panic!("embedded spec {filename} failed to parse: {e}"));
            let expected = (parsed.name != stem).then_some(parsed.name.as_str());
            assert_eq!(
                name_alias, expected,
                "embedded alias manifest disagrees with parsed CompletionSpec.name for {filename}"
            );
        }
    }

    #[test]
    fn load_with_embedded_resolves_non_stem_name_alias() {
        let result = crate::specs::SpecStore::load_with_embedded(&[])
            .expect("embedded corpus must register");
        let store = result.store;

        let by_stem = store
            .get("appwrite")
            .expect("appwrite stem must resolve through embedded corpus");
        assert_eq!(by_stem.name, "index");

        let by_alias = store
            .get("index")
            .expect("appwrite name alias must resolve through embedded corpus");
        assert_eq!(by_alias.name, "index");
        assert_eq!(
            by_stem.as_ref() as *const _,
            by_alias.as_ref() as *const _,
            "stem and name alias must resolve to the same parsed embedded spec"
        );
    }

    #[test]
    fn embedded_spec_contents_round_trips() {
        // Cache invariant: the same filename must return the identical
        // pointer on every call. We can no longer compare against a
        // baked-in body (we don't have one), so we assert the lazy cache
        // is pointer-stable instead — that is the load-bearing property
        // for `Cow::Borrowed` lifetimes inside `parse_entry_source`.
        for filename in embedded_filenames() {
            let first = embedded_spec_contents(filename).expect("filename listed must resolve");
            let second = embedded_spec_contents(filename).expect("filename listed must resolve");
            assert_eq!(
                first.as_ptr(),
                second.as_ptr(),
                "embedded_spec_contents must be pointer-stable across calls for {filename}"
            );
            assert!(
                !first.is_empty(),
                "embedded spec {filename} decoded to empty body"
            );
        }
        assert!(embedded_spec_contents("does-not-exist.json").is_none());
    }

    #[test]
    fn embedded_specs_table_non_empty() {
        // If this ever fails it means the archive header reported zero
        // entries — the runtime fallback would silently load zero specs,
        // defeating the binary-embedded fallback entirely.
        assert!(
            embedded_spec_count() > 0,
            "embedded spec count must be greater than zero"
        );
        // Sanity: every filename must be non-empty and end in `.json`.
        for filename in embedded_filenames() {
            assert!(!filename.is_empty(), "embedded spec has empty filename");
            assert!(
                filename.ends_with(".json"),
                "embedded spec {filename} should be a .json file"
            );
        }
    }

    #[test]
    fn embedded_specs_preserve_corrected_in_markers() {
        // `build.rs::strip_legacy_js_source` must NOT strip `_corrected_in`.
        // That marker surfaces generators that were previously mis-converted.
        //
        // WHY this test pins a total: a build-time regression that
        // incidentally dropped `_corrected_in` at any nesting depth would
        // not fail any existing test — every spec would still parse, every
        // generator would still load, but `ghost-complete doctor` would go
        // silent. Walk every embedded spec, count markers, compare to the
        // expected total.
        //
        // Expected count is hard-coded rather than derived from
        // `docs/coverage-baseline.json` because (a) the baseline file is
        // outside this crate and the test would then depend on the workspace
        // layout, and (b) the baseline's `corrected_generators` field is a
        // release-time snapshot that lags the live spec set. If you add or
        // remove `_corrected_in` markers in `specs/`, update this constant
        // and the baseline together in the same PR.
        const EXPECTED_CORRECTED_IN: usize = 192;

        fn count(v: &serde_json::Value) -> usize {
            match v {
                serde_json::Value::Object(map) => {
                    let here = usize::from(map.contains_key("_corrected_in"));
                    here + map.values().map(count).sum::<usize>()
                }
                serde_json::Value::Array(arr) => arr.iter().map(count).sum(),
                _ => 0,
            }
        }

        let total: usize = embedded_filenames()
            .iter()
            .map(|filename| {
                let body = embedded_spec_contents(filename)
                    .expect("filename listed must resolve to a body");
                let v: serde_json::Value = serde_json::from_str(body)
                    .unwrap_or_else(|e| panic!("embedded spec {filename} is not valid JSON: {e}"));
                count(&v)
            })
            .sum();

        assert_eq!(
            total, EXPECTED_CORRECTED_IN,
            "embedded specs have {total} `_corrected_in` markers, expected {EXPECTED_CORRECTED_IN}. \
             If you changed a corrected generator, update EXPECTED_CORRECTED_IN and \
             docs/coverage-baseline.json together. If you did NOT intentionally change \
             corrections, build.rs::strip_legacy_js_source likely dropped the marker — check its \
             recursion."
        );
    }

    #[test]
    fn purge_embedded_cache_at_removes_directory() {
        let tmp = TempDir::new().unwrap();
        let cache = tmp.path().join("embedded-specs");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("git.json"), "{}").unwrap();

        let purged = purge_embedded_cache_at(&cache).unwrap();

        assert_eq!(purged.as_deref(), Some(cache.as_path()));
        assert!(
            !cache.exists(),
            "legacy embedded cache directory must be removed"
        );
    }

    #[test]
    fn purge_embedded_cache_at_returns_none_when_dir_absent() {
        let tmp = TempDir::new().unwrap();
        let absent = tmp.path().join("never-existed");

        let purged = purge_embedded_cache_at(&absent).unwrap();

        assert!(purged.is_none());
        assert!(!absent.exists());
    }

    #[test]
    fn purge_embedded_cache_at_ignores_non_directory() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("embedded-specs");
        std::fs::write(&file, "not a directory").unwrap();

        let purged = purge_embedded_cache_at(&file).unwrap();

        assert!(purged.is_none());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "not a directory");
    }

    /// A symlink at the cache-dir path must be left intact — `remove_dir_all`
    /// would otherwise delete files inside an attacker-controlled target.
    #[test]
    #[cfg(unix)]
    fn purge_embedded_cache_at_refuses_to_follow_symlink() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let canary = real.join("canary.txt");
        std::fs::write(&canary, "UNTOUCHED").unwrap();

        let link = tmp.path().join("link-cache");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let purged = purge_embedded_cache_at(&link).unwrap();

        assert!(purged.is_none());
        assert!(canary.exists());
        let after = std::fs::symlink_metadata(&link).unwrap();
        assert!(after.file_type().is_symlink());
    }
}
