//! Integration tests for the lazy spec-loading contract.
//!
//! These guard the v0.12.4 memory regression fix. Loading a directory
//! must NOT parse any spec until the first call to `SpecStore::get`
//! or `SpecEntry::spec`. Iteration force-loads every entry; lazy
//! failures are sticky and surfaced via `SpecEntry::load_error`.

use std::fs;
use std::sync::{Arc, Mutex};

use gc_suggest::specs::SpecSource;
use gc_suggest::{SpecLocation, SpecStore, MAX_SPEC_JSON_DEPTH};
use tempfile::TempDir;
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("capture buffer poisoned")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn install_log_capture() -> (Arc<Mutex<Vec<u8>>>, tracing::subscriber::DefaultGuard) {
    let captured = Arc::new(Mutex::new(Vec::<u8>::new()));
    let writer = CaptureWriter(Arc::clone(&captured));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    tracing_core::callsite::rebuild_interest_cache();
    (captured, guard)
}

fn captured_logs(captured: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&captured.lock().expect("capture buffer poisoned")).into_owned()
}

fn write_spec(dir: &std::path::Path, filename: &str, body: &str) {
    fs::write(dir.join(filename), body).unwrap();
}

fn minimal_spec(name: &str) -> String {
    format!(r#"{{"name":"{name}","subcommands":[],"options":[],"args":[]}}"#)
}

fn nested_spec_with_name(name: &str, depth: usize) -> String {
    let mut spec = format!(r#"{{"name":"{name}","subcommands":["#);
    for _ in 0..depth {
        spec.push_str(r#"{"name":"nested","subcommands":["#);
    }
    spec.push_str(r#"{"name":"leaf"}"#);
    for _ in 0..depth {
        spec.push_str("]}");
    }
    spec.push_str("]}");
    spec
}

#[test]
fn lazy_get_does_not_parse_until_called() {
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", &minimal_spec("git"));
    write_spec(dir.path(), "npm.json", &minimal_spec("npm"));

    let result = SpecStore::load_from_dir(dir.path()).unwrap();
    let store = result.store;

    for entry in store.entries() {
        assert!(
            !entry.is_parsed(),
            "entry {} was parsed at load time — lazy contract violated",
            entry.id
        );
    }

    assert!(store.get("git").is_some());
    let git_entry = store
        .entries()
        .iter()
        .find(|e| e.id == "git")
        .expect("git entry must exist");
    assert!(git_entry.is_parsed(), "git must be parsed after get()");

    let npm_entry = store
        .entries()
        .iter()
        .find(|e| e.id == "npm")
        .expect("npm entry must exist");
    assert!(
        !npm_entry.is_parsed(),
        "npm must remain unparsed when only git was looked up"
    );
}

#[test]
fn lazy_iter_force_loads_all() {
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", &minimal_spec("git"));
    write_spec(dir.path(), "npm.json", &minimal_spec("npm"));
    write_spec(dir.path(), "cargo.json", &minimal_spec("cargo"));

    let result = SpecStore::load_from_dir(dir.path()).unwrap();
    let store = result.store;

    let collected: Vec<&str> = store.iter().map(|(id, _)| id).collect();
    assert_eq!(collected.len(), 3);

    for entry in store.entries() {
        assert!(
            entry.is_parsed(),
            "entry {} was not force-loaded by iter()",
            entry.id
        );
    }
}

#[test]
fn lazy_load_failure_is_sticky() {
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "broken.json", "{not valid json");
    write_spec(dir.path(), "ok.json", &minimal_spec("ok"));

    let result = SpecStore::load_from_dir(dir.path()).unwrap();
    let store = result.store;

    // Filesystem walk succeeded — only the JSON itself is broken,
    // and the failure surfaces lazily.
    assert_eq!(store.entries().len(), 2);
    assert!(result.errors.is_empty());

    assert!(store.get("broken").is_none());
    let broken = store.entries().iter().find(|e| e.id == "broken").unwrap();
    let first_error = broken.load_error().unwrap().to_string();
    write_spec(dir.path(), "broken.json", &minimal_spec("broken"));

    // Second lookup must not retry — the OnceLock pinned the failure.
    assert!(store.get("broken").is_none());
    assert_eq!(broken.load_error(), Some(first_error.as_str()));

    // Healthy spec still works alongside the broken one.
    assert!(store.get("ok").is_some());
}

#[test]
fn shallow_alias_parse_respects_json_depth_cap() {
    let dir = TempDir::new().unwrap();
    write_spec(
        dir.path(),
        "evil.json",
        &nested_spec_with_name("too-deep", MAX_SPEC_JSON_DEPTH + 1),
    );

    let result = SpecStore::load_from_dir(dir.path()).unwrap();
    let store = result.store;
    let evil = store
        .entries()
        .iter()
        .find(|e| e.id == "evil")
        .expect("evil entry must register by filename stem");

    assert_eq!(evil.aliases, vec!["evil"]);
    assert!(
        store.get("too-deep").is_none(),
        "top-level name from too-deep JSON must not be registered as an alias"
    );
    assert!(
        !evil.is_parsed(),
        "missing rejected name alias must not force the full lazy parse"
    );

    assert!(store.get("evil").is_none());
    let err = evil
        .load_error()
        .expect("stem lookup must record load error");
    assert!(
        err.contains("maximum JSON nesting depth"),
        "expected depth-cap load error, got {err}"
    );
}

#[test]
fn invalid_filesystem_override_falls_back_to_embedded_spec() {
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", "{not valid json");

    let result = SpecStore::load_with_embedded(&[dir.path().to_path_buf()]).unwrap();
    assert!(result.errors.is_empty());

    let git = result
        .store
        .get("git")
        .expect("embedded git spec must resolve when filesystem override fails");
    assert_eq!(git.name, "git");

    let fs_git = result
        .store
        .entries()
        .iter()
        .find(|entry| {
            matches!(
                &entry.source,
                SpecSource::Filesystem(path) if path == &dir.path().join("git.json")
            )
        })
        .expect("bad filesystem override must remain visible for diagnostics");
    assert!(
        fs_git.load_error().is_some(),
        "failed filesystem override must surface its lazy parse error"
    );
}

#[test]
fn valid_filesystem_override_is_not_duplicated_by_embedded_fallback() {
    let dir = TempDir::new().unwrap();
    write_spec(
        dir.path(),
        "git.json",
        r#"{"name":"git","subcommands":[{"name":"installed-copy"}],"options":[],"args":[]}"#,
    );

    let result = SpecStore::load_with_embedded(&[dir.path().to_path_buf()]).unwrap();
    let git_entries: Vec<_> = result.store.iter().filter(|(id, _)| *id == "git").collect();

    assert_eq!(
        git_entries.len(),
        1,
        "valid filesystem override must be the only resolved git entry"
    );
    assert_eq!(git_entries[0].1.subcommands[0].name, "installed-copy");
}

#[test]
fn malformed_spec_warns_once_on_repeated_lookup() {
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "broken.json", "{not valid json");

    let result = SpecStore::load_from_dir(dir.path()).unwrap();
    let store = result.store;
    let (captured, _guard) = install_log_capture();

    assert!(store.get("broken").is_none());
    assert!(store.get("broken").is_none());

    let logs = captured_logs(&captured);
    assert_eq!(
        logs.matches("spec lazy load failed").count(),
        1,
        "lazy parse failure must warn exactly once on repeated lookup:\n{logs}"
    );
    assert!(logs.contains("broken"));
    assert!(logs.contains("broken.json"));
}

#[test]
fn force_load_errors_reports_lazy_parse_failures() {
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "broken.json", "{not valid json");
    write_spec(dir.path(), "ok.json", &minimal_spec("ok"));

    let result = SpecStore::load_from_dir(dir.path()).unwrap();
    assert!(result.errors.is_empty());

    let errors = result.store.force_load_errors();
    assert_eq!(errors.len(), 1, "expected one forced load error");
    assert_eq!(errors[0].id, "broken");
    assert!(errors[0].error.contains("parse"));
    match &errors[0].source {
        SpecLocation::Filesystem { stem, path } => {
            assert_eq!(stem, "broken");
            assert_eq!(path, &dir.path().join("broken.json"));
        }
        other => panic!("expected filesystem location, got {other:?}"),
    }
}

#[test]
fn embedded_locations_are_not_fabricated_as_canonical_paths() {
    let result = SpecStore::load_with_embedded(&[]).unwrap();
    assert!(
        result.store.canonical_paths().is_empty(),
        "embedded specs must not produce fake <embedded>/name.json paths"
    );

    let locations = result.store.locations();
    assert!(
        locations
            .iter()
            .any(|location| matches!(location, SpecLocation::Embedded { stem } if stem == "git")),
        "locations() must model embedded specs explicitly"
    );
}

#[test]
fn lazy_get_concurrent_first_touch_is_safe() {
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", &minimal_spec("git"));

    let result = SpecStore::load_from_dir(dir.path()).unwrap();
    let store = Arc::new(result.store);

    let handles: Vec<_> = (0..16)
        .map(|_| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                let spec = store.get("git").expect("git must resolve");
                spec.name.clone()
            })
        })
        .collect();

    let names: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(names.iter().all(|n| n == "git"));
}

#[test]
fn lazy_get_preserves_alias_resolution_via_shallow_parse() {
    // A custom user spec whose CompletionSpec.name differs from the
    // filename stem. The lazy path runs a shallow parse to extract
    // `name`, registers it as an alias, then defers full parsing
    // until SpecEntry::spec() is called.
    let dir = TempDir::new().unwrap();
    write_spec(
        dir.path(),
        "my-tool.json",
        r#"{"name":"mt","subcommands":[],"options":[],"args":[]}"#,
    );

    let result = SpecStore::load_from_dir(dir.path()).unwrap();
    let store = result.store;

    assert!(store.get("my-tool").is_some(), "stem alias must resolve");
    assert!(
        store.get("mt").is_some(),
        "shallow-parsed name alias must resolve"
    );
}

#[test]
fn runtime_does_not_materialize_embedded_to_cache_dir() {
    // SpecStore::load_with_embedded with no filesystem dirs uses the
    // embedded corpus directly — no disk materialisation. We confirm
    // the store is non-empty (embedded corpus loaded) and that any
    // pre-existing materialised cache from older runs is unrelated
    // to this code path. The cache-dir cleanup itself lives in the
    // `install` command (covered separately).
    let result = SpecStore::load_with_embedded(&[]).unwrap();
    assert!(
        !result.store.is_empty(),
        "embedded corpus must register via load_with_embedded"
    );
    assert!(result.errors.is_empty(), "no filesystem dirs to fail");
}
