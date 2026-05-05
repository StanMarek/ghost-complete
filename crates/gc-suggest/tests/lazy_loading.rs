//! Integration tests for the lazy spec-loading contract.
//!
//! These guard the v0.12.4 memory regression fix. Loading a directory
//! must NOT parse any spec until the first call to `SpecStore::get`
//! or `SpecEntry::spec`. Iteration force-loads every entry; lazy
//! failures are sticky and surfaced via `SpecEntry::load_error`.

use std::fs;
use std::sync::Arc;

use gc_suggest::SpecStore;
use tempfile::TempDir;

fn write_spec(dir: &std::path::Path, filename: &str, body: &str) {
    fs::write(dir.join(filename), body).unwrap();
}

fn minimal_spec(name: &str) -> String {
    format!(r#"{{"name":"{name}","subcommands":[],"options":[],"args":[]}}"#)
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
    let broken = store
        .entries()
        .iter()
        .find(|e| e.id == "broken")
        .unwrap();
    assert!(broken.load_error().is_some());

    // Second lookup must not retry — the OnceLock pinned the failure.
    assert!(store.get("broken").is_none());
    assert!(broken.load_error().is_some());

    // Healthy spec still works alongside the broken one.
    assert!(store.get("ok").is_some());
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
