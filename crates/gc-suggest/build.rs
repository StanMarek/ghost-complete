//! Preprocess embedded completion specs at build time.
//!
//! Reads every `../../specs/*.json`, defensively drops any straggler
//! `js_source` field from generators (no-op for freshly-converted specs;
//! catches stale user-installed specs from older converter versions),
//! serialises the result compactly (no pretty-print whitespace), zstd-
//! compresses each spec at level 19, and packs every entry into a single
//! binary archive `$OUT_DIR/embedded_specs.bin` that the runtime parses
//! lazily on first lookup.
//!
//! The on-disk `specs/` directory stays unchanged — tests, fixtures,
//! and the converter keep reading the human-readable pretty-printed
//! copies. Only the binary-embedded archive is compressed.
//!
//! ## Why this exists
//!
//! Binary-size intervention. The original embedded pattern baked 21 MB
//! of pretty-printed JSON directly via `include_str!`, which landed as
//! ~42 MB of `__const` data in the release binary (each whitespace byte
//! round-trips verbatim through rustc). Minifying dropped that to ~47 MB
//! of source bytes after the AWS restoration (ux-8). Layering zstd-19
//! compression on top reclaims ~30 MB of stripped binary size, unblocking
//! ux-13 which needs the headroom for typed AWS SDK crates.
//!
//! ## Archive format (`embedded_specs.bin`)
//!
//! Little-endian throughout. Entries appear in sorted-filename order.
//!
//! ```text
//! [u32 entry_count]
//! per entry:
//!   [u16 filename_len][filename UTF-8 bytes]
//!   [u8  has_alias]                              ; 0 or 1
//!   if has_alias == 1:
//!     [u16 alias_len][alias UTF-8 bytes]
//!   [u32 blob_len][zstd-compressed JSON bytes]
//! ```
//!
//! The runtime memory-maps the archive as a `&'static [u8]` slice via
//! `include_bytes!`, parses the header once into an in-memory index, and
//! lazily zstd-decompresses each spec body on first lookup. Filenames and
//! aliases are tiny (~7 KB total) and live in the index uncompressed; only
//! JSON bodies are compressed.
//!
//! ## Invariants
//!
//! - Entries are emitted in **sorted filename order** so the runtime can
//!   binary-search or expose `embedded_filenames()` as a stable iteration
//!   target.
//! - `_corrected_in` is intentionally NOT stripped — it is consumed at
//!   runtime by `ghost-complete doctor` to surface generators that
//!   previously mis-converted.
//! - `js_runtime` survives the strip pass — the runtime reads its
//!   `source` field to drive the QuickJS evaluator. The post-strip
//!   assertion below catches a regression where a future stripper
//!   accidentally walks into the runtime metadata.
//! - If a spec is not valid JSON, we bail loudly rather than silently
//!   emit the broken source — a malformed spec in the binary would
//!   manifest as a load-time parse error with no hint why.
//! - `rerun-if-changed` is emitted for the specs directory so cargo
//!   reruns this script whenever any spec is added/removed/edited.

use std::env;
use std::fs;
use std::path::PathBuf;

/// zstd compression level. Level 19 is the highest level that still uses
/// the regular compressor; levels 20-22 require explicit `--long` window
/// settings and are not a net win on small JSON specs.
const ZSTD_LEVEL: i32 = 19;

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let specs_dir = manifest_dir.join("../../specs").canonicalize().expect(
        "specs/ directory must exist relative to crates/gc-suggest/ \
         (check the workspace layout)",
    );
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    // Tell cargo to rerun whenever the source specs change. Watching
    // the directory alone would miss modifications to individual files
    // that don't change the dir's mtime on some filesystems, so emit a
    // per-file watch too.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", specs_dir.display());

    let mut specs: Vec<(String, PathBuf)> = Vec::new();
    for entry in fs::read_dir(&specs_dir).expect("read_dir specs/") {
        let entry = entry.expect("read_dir entry");
        let path = entry.path();
        // Skip non-JSON and the `__snapshots__/` subdirectory (insta
        // golden snapshots — not specs).
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("spec file has valid UTF-8 name")
            .to_string();
        println!("cargo:rerun-if-changed={}", path.display());
        specs.push((name, path));
    }
    // Sort for deterministic output (otherwise the order depends on
    // read_dir's filesystem-specific enumeration).
    specs.sort_by(|a, b| a.0.cmp(&b.0));

    // Process each spec: parse, strip any legacy js_source straggler,
    // assert js_runtime.source survives the strip, re-serialise compactly,
    // zstd-compress. `entries` accumulates `(filename, name_alias, blob)`
    // tuples that the archive writer emits in order.
    type Entry = (String, Option<String>, Vec<u8>);
    let mut entries: Vec<Entry> = Vec::with_capacity(specs.len());
    for (name, src_path) in &specs {
        let src = fs::read_to_string(src_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", src_path.display()));
        let mut value: serde_json::Value = serde_json::from_str(&src)
            .unwrap_or_else(|e| panic!("parse {}: {e}", src_path.display()));
        strip_legacy_js_source(&mut value);
        // Sanity: the strip must NOT have touched js_runtime.source. The
        // runtime reads it to drive the JS evaluator; an accidental strip
        // would silently disable JS-driven generators across the corpus.
        assert_js_runtime_source_preserved(&value, src_path);

        // Capture the spec's declared `name` if it differs from the
        // filename stem. Stored alongside the filename in the archive so
        // the runtime alias index can register `CompletionSpec.name`
        // aliases without parsing JSON at startup.
        let declared_name = value
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let stem = name
            .strip_suffix(".json")
            .expect("emitted entries always end with .json")
            .to_owned();
        let name_alias = match declared_name {
            Some(n) if n != stem => Some(n),
            _ => None,
        };

        let minified = serde_json::to_string(&value)
            .unwrap_or_else(|e| panic!("serialize {}: {e}", src_path.display()));

        let blob = zstd::encode_all(minified.as_bytes(), ZSTD_LEVEL)
            .unwrap_or_else(|e| panic!("zstd encode {}: {e}", src_path.display()));

        entries.push((name.clone(), name_alias, blob));
    }

    // Write the binary archive. The format is intentionally simple — no
    // CRC, no version byte — because the producer and consumer ship as a
    // single artifact: a corrupt archive can only arise from a build-script
    // bug, in which case loud panics in the parser are the right answer.
    let archive_path = out_dir.join("embedded_specs.bin");
    let mut bytes: Vec<u8> = Vec::with_capacity(1024 * 1024);
    let entry_count: u32 = entries
        .len()
        .try_into()
        .expect("entry count exceeds u32 — refusing to silently truncate");
    bytes.extend_from_slice(&entry_count.to_le_bytes());
    for (filename, name_alias, blob) in &entries {
        write_short_string(&mut bytes, filename);
        match name_alias {
            Some(alias) => {
                bytes.push(1);
                write_short_string(&mut bytes, alias);
            }
            None => bytes.push(0),
        }
        let blob_len: u32 = blob
            .len()
            .try_into()
            .expect("compressed blob exceeds u32 — refusing to silently truncate");
        bytes.extend_from_slice(&blob_len.to_le_bytes());
        bytes.extend_from_slice(blob);
    }
    fs::write(&archive_path, &bytes)
        .unwrap_or_else(|e| panic!("write {}: {e}", archive_path.display()));
}

/// Append `[u16 len][bytes]` to `out`. Panics if `s` exceeds `u16::MAX`
/// bytes (no realistic filename or alias comes anywhere close).
fn write_short_string(out: &mut Vec<u8>, s: &str) {
    let len: u16 = s
        .len()
        .try_into()
        .expect("filename or alias exceeds u16::MAX bytes");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Strip the legacy `js_source` field from generators in-place.
///
/// `js_source` has been replaced by structured `js_runtime.source` metadata
/// that the runtime can actually consume. This stripper is defensive: it
/// drops `js_source` from stale user-installed specs (copied from older
/// converter versions) so the embedded format stays consistent. For
/// freshly-converted specs this is a no-op.
fn strip_legacy_js_source(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(map) => {
            map.remove("js_source");
            for (_, child) in map.iter_mut() {
                strip_legacy_js_source(child);
            }
        }
        serde_json::Value::Array(arr) => {
            for child in arr.iter_mut() {
                strip_legacy_js_source(child);
            }
        }
        _ => {}
    }
}

/// Walk the spec tree and panic if any `js_runtime` object lacks a non-empty
/// string `source` field. Belt-and-braces against a future strip pass that
/// accidentally walks into the runtime metadata: the runtime can't drive a
/// generator with no source body, so an undetected strip would silently
/// regress every JS-driven generator in the embedded corpus.
fn assert_js_runtime_source_preserved(v: &serde_json::Value, path: &std::path::Path) {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::Object(jr)) = map.get("js_runtime") {
                match jr.get("source") {
                    Some(serde_json::Value::String(s)) if !s.is_empty() => {}
                    other => panic!(
                        "{}: js_runtime.source missing or empty after strip pass — got {:?}",
                        path.display(),
                        other
                    ),
                }
            }
            for child in map.values() {
                assert_js_runtime_source_preserved(child, path);
            }
        }
        serde_json::Value::Array(arr) => {
            for child in arr {
                assert_js_runtime_source_preserved(child, path);
            }
        }
        _ => {}
    }
}
