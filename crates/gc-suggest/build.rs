//! Preprocess embedded completion specs at build time.
//!
//! Reads every `../../specs/*.json`, defensively drops any straggler
//! `js_source` field from generators (no-op for freshly-converted specs;
//! catches stale user-installed specs from older converter versions),
//! serialises the result compactly (no pretty-print whitespace) into
//! `$OUT_DIR/specs-min/`, and writes an `embedded_specs.rs` include that
//! the `embedded` module `include!`s.
//!
//! The on-disk `specs/` directory stays unchanged — tests, fixtures,
//! and the converter keep reading the human-readable pretty-printed
//! copies. Only the binary-embedded copies are shrunk.
//!
//! ## Why this exists
//!
//! Binary-size intervention. The original embedded pattern baked 21 MB
//! of pretty-printed JSON directly via `include_str!`, which landed as
//! ~42 MB of `__const` data in the release binary (each whitespace byte
//! round-trips verbatim through rustc). Minifying drops that to ~11 MB
//! of source bytes. Stripping the legacy `js_source` field shaved
//! another ~435 KB; that data is now carried on `js_runtime.source` so
//! the runtime can evaluate it — `js_source` itself stays stripped for
//! compatibility with stale user-installed specs.
//!
//! ## Invariants
//!
//! - The emitted `EMBEDDED_SPECS` list preserves the exact filename keys
//!   from `specs/` (so `write_embedded_specs` still materialises a
//!   directory that the on-disk spec loader can re-read).
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
use std::io::Write;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let specs_dir = manifest_dir.join("../../specs").canonicalize().expect(
        "specs/ directory must exist relative to crates/gc-suggest/ \
         (check the workspace layout)",
    );
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let out_specs = out_dir.join("specs-min");
    fs::create_dir_all(&out_specs).expect("create OUT_DIR/specs-min");

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
    // assert js_runtime.source survives the strip, re-serialise compactly.
    let mut entries: Vec<(String, PathBuf)> = Vec::with_capacity(specs.len());
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
        let minified = serde_json::to_string(&value)
            .unwrap_or_else(|e| panic!("serialize {}: {e}", src_path.display()));

        let dest = out_specs.join(name);
        fs::write(&dest, minified.as_bytes())
            .unwrap_or_else(|e| panic!("write {}: {e}", dest.display()));
        entries.push((name.clone(), dest));
    }

    // Emit the generated `embedded_specs.rs` with the EMBEDDED_SPECS
    // const. The `embedded` module picks this file up via
    // `include!(concat!(env!("OUT_DIR"), "/embedded_specs.rs"))`.
    let embed_rs = out_dir.join("embedded_specs.rs");
    let mut f = fs::File::create(&embed_rs).expect("create embedded_specs.rs");
    writeln!(f, "// @generated by build.rs — do not edit").unwrap();
    writeln!(f, "pub const EMBEDDED_SPECS: &[(&str, &str)] = &[").unwrap();
    for (name, path) in &entries {
        // Escape both the name and the path as Rust string literals.
        // `name` comes from filesystem filenames we already validated
        // as UTF-8; paths come from OUT_DIR which the Rust toolchain
        // produces. Using debug formatting for the path guarantees
        // correct escaping on platforms with unusual characters (though
        // we only target macOS, this keeps the output robust).
        writeln!(
            f,
            "    ({:?}, include_str!({:?})),",
            name,
            path.display().to_string()
        )
        .unwrap();
    }
    writeln!(f, "];").unwrap();
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
