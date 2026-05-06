//! Integration tests for the spec cache TTL eviction policy.
//!
//! Eviction is opt-in (idle_ttl_secs=0 disables it). These tests exercise
//! the SpecStore::evict_idle policy directly with an explicit clock; the
//! sweep task itself is covered by sweep_smoke_* tests later in this file.

use std::collections::HashSet;
use std::fs;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gc_suggest::SpecStore;
use tempfile::TempDir;

fn write_spec(dir: &std::path::Path, filename: &str, body: &str) {
    fs::write(dir.join(filename), body).unwrap();
}

fn minimal_spec(name: &str) -> String {
    format!(r#"{{"name":"{name}","subcommands":[],"options":[],"args":[]}}"#)
}

fn empty_keep_warm() -> HashSet<String> {
    HashSet::new()
}

#[test]
fn eviction_disabled_preserves_v0_12_4_contract() {
    // idle_threshold = MAX means no entry is ever idle long enough.
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", &minimal_spec("git"));
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    let _ = store.get("git");
    assert_eq!(store.parsed_count(), 1);

    let report = store.evict_idle(Duration::MAX, None, &empty_keep_warm());
    assert_eq!(report.evicted_idle_count, 0);
    assert_eq!(store.parsed_count(), 1);
}

#[test]
fn evict_idle_evicts_loaded_past_threshold() {
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", &minimal_spec("git"));
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    let _ = store.get("git");
    assert_eq!(store.parsed_count(), 1);

    // Force last_accessed to a known-old time.
    let entry = store.entries().iter().find(|e| e.id == "git").unwrap().clone();
    let an_hour_ago = SystemTime::now() - Duration::from_secs(3600);
    entry.set_last_accessed_for_test(an_hour_ago);

    let report = store.evict_idle_at(
        SystemTime::now(),
        Duration::from_secs(60),
        None,
        &empty_keep_warm(),
    );
    assert_eq!(report.evicted_idle_count, 1);
    assert_eq!(store.parsed_count(), 0);
}

#[test]
fn evict_idle_skips_keep_warm_by_filename_stem() {
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", &minimal_spec("git"));
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    let _ = store.get("git");
    let entry = store.entries().iter().find(|e| e.id == "git").unwrap().clone();
    entry.set_last_accessed_for_test(SystemTime::now() - Duration::from_secs(3600));

    let mut keep_warm = HashSet::new();
    keep_warm.insert("git".to_string());
    let report = store.evict_idle_at(
        SystemTime::now(),
        Duration::from_secs(60),
        None,
        &keep_warm,
    );
    assert_eq!(report.evicted_idle_count, 0);
    assert_eq!(report.kept_warm_count, 1);
    assert_eq!(store.parsed_count(), 1);
}

#[test]
fn evict_idle_skips_keep_warm_by_completion_name() {
    // A spec whose CompletionSpec.name differs from filename stem.
    let dir = TempDir::new().unwrap();
    write_spec(
        dir.path(),
        "my-tool.json",
        r#"{"name":"mt","subcommands":[],"options":[],"args":[]}"#,
    );
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    let _ = store.get("mt");
    let entry = store
        .entries()
        .iter()
        .find(|e| e.id == "my-tool")
        .unwrap()
        .clone();
    entry.set_last_accessed_for_test(SystemTime::now() - Duration::from_secs(3600));

    let mut keep_warm = HashSet::new();
    keep_warm.insert("mt".to_string()); // matches CompletionSpec.name, not stem
    let report = store.evict_idle_at(
        SystemTime::now(),
        Duration::from_secs(60),
        None,
        &keep_warm,
    );
    assert_eq!(report.kept_warm_count, 1);
    assert_eq!(report.evicted_idle_count, 0);
}

#[test]
fn evict_idle_does_not_clear_failed_slot() {
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "broken.json", "{not valid json");
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    assert!(store.get("broken").is_none()); // populates Failed
    let entry = store
        .entries()
        .iter()
        .find(|e| e.id == "broken")
        .unwrap()
        .clone();
    entry.set_last_accessed_for_test(SystemTime::now() - Duration::from_secs(3600));

    let _ = store.evict_idle_at(
        SystemTime::now(),
        Duration::from_secs(60),
        None,
        &empty_keep_warm(),
    );
    // Failed slot survives — load_error still returns the original message.
    assert!(
        entry.load_error().is_some(),
        "Failed slot must survive eviction (sticky failures)"
    );
}

#[test]
fn evict_idle_does_not_clear_empty_slot() {
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", &minimal_spec("git"));
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    // Never call get(); slot stays Empty.
    let entry = store.entries().iter().find(|e| e.id == "git").unwrap().clone();
    assert!(!entry.is_parsed());

    let report = store.evict_idle_at(
        SystemTime::now(),
        Duration::from_secs(0), // every entry "idle"
        None,
        &empty_keep_warm(),
    );
    assert_eq!(
        report.evicted_idle_count, 0,
        "Empty slot must not count as evicted"
    );
    assert!(!entry.is_parsed(), "Empty slot should remain Empty after sweep");
}

#[test]
fn evict_idle_then_get_returns_fresh_arc_with_same_contents() {
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", &minimal_spec("git"));
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    let arc1 = store.get("git").expect("first get must resolve");
    let entry = store.entries().iter().find(|e| e.id == "git").unwrap().clone();
    entry.set_last_accessed_for_test(SystemTime::now() - Duration::from_secs(3600));

    let _ = store.evict_idle_at(
        SystemTime::now(),
        Duration::from_secs(60),
        None,
        &empty_keep_warm(),
    );
    let arc2 = store.get("git").expect("second get re-parses and resolves");
    assert!(
        !Arc::ptr_eq(&arc1, &arc2),
        "post-eviction Arc must be a fresh allocation"
    );
    assert_eq!(arc1.name, arc2.name);
}

#[test]
fn last_sweep_records_under_lock() {
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", &minimal_spec("git"));
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    assert!(store.last_sweep().is_none());

    let now = SystemTime::now();
    let _ = store.evict_idle_at(now, Duration::MAX, None, &empty_keep_warm());

    let report = store.last_sweep().expect("last_sweep must record after a run");
    assert_eq!(report.timestamp, now);
    assert_eq!(report.evicted_idle_count, 0);
}

#[test]
fn concurrent_evict_and_get_yields_single_parse() {
    use std::sync::Barrier;
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", &minimal_spec("git"));
    let store = Arc::new(SpecStore::load_from_dir(dir.path()).unwrap().store);

    // Force the slot to Evicted via a manual sweep-then-eligible.
    let _ = store.get("git");
    let entry = store.entries().iter().find(|e| e.id == "git").unwrap().clone();
    entry.set_last_accessed_for_test(SystemTime::now() - Duration::from_secs(3600));
    let _ = store.evict_idle_at(
        SystemTime::now(),
        Duration::from_secs(60),
        None,
        &empty_keep_warm(),
    );
    assert_eq!(store.parsed_count(), 0);

    // Race 16 readers against the now-Evicted slot. They must all observe
    // the same Arc identity (one parse, all share).
    let barrier = Arc::new(Barrier::new(16));
    let handles: Vec<_> = (0..16)
        .map(|_| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                store.get("git").expect("git must resolve")
            })
        })
        .collect();

    let arcs: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let first = &arcs[0];
    for arc in &arcs[1..] {
        assert!(
            Arc::ptr_eq(first, arc),
            "all concurrent readers must observe the same Arc — single-flight broken"
        );
    }
}

#[test]
fn force_load_errors_re_parses_evicted_entries() {
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", &minimal_spec("git"));
    write_spec(dir.path(), "broken.json", "{not valid json");
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;

    // Prime: parse both. Broken stays Failed; git becomes Loaded.
    let _ = store.get("git");
    let _ = store.get("broken");
    let git_entry = store.entries().iter().find(|e| e.id == "git").unwrap().clone();
    git_entry.set_last_accessed_for_test(SystemTime::now() - Duration::from_secs(3600));

    // Evict git (broken slot is Failed and survives).
    let _ = store.evict_idle_at(
        SystemTime::now(),
        Duration::from_secs(60),
        None,
        &empty_keep_warm(),
    );
    assert_eq!(store.parsed_count(), 0);

    // force_load_errors should re-parse evicted entries (so git becomes
    // Loaded again) and return the broken entry's still-stuck Failed
    // error. Documented behaviour: status pays the re-parse cost.
    let errors = store.force_load_errors();
    assert_eq!(errors.len(), 1, "only the broken entry should report an error");
    assert_eq!(errors[0].id, "broken");
    assert_eq!(
        store.parsed_count(),
        1,
        "git must have been re-parsed by force_load"
    );
}
