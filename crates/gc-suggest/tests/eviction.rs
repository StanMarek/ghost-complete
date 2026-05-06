//! Integration tests for the spec cache TTL eviction policy.
//!
//! Eviction is opt-in (idle_ttl_secs=0 disables it). These tests exercise
//! the SpecStore::evict_idle policy directly with an explicit clock; the
//! sweep task itself is covered by sweep_smoke_* tests later in this file.

use std::collections::HashSet;
use std::fs;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use gc_config::SpecCacheConfig;
use gc_suggest::SpecStore;
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
    let entry = store
        .entries()
        .iter()
        .find(|e| e.id == "git")
        .unwrap()
        .clone();
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
    let entry = store
        .entries()
        .iter()
        .find(|e| e.id == "git")
        .unwrap()
        .clone();
    entry.set_last_accessed_for_test(SystemTime::now() - Duration::from_secs(3600));

    let mut keep_warm = HashSet::new();
    keep_warm.insert("git".to_string());
    let report = store.evict_idle_at(SystemTime::now(), Duration::from_secs(60), None, &keep_warm);
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
    let report = store.evict_idle_at(SystemTime::now(), Duration::from_secs(60), None, &keep_warm);
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
    let entry = store
        .entries()
        .iter()
        .find(|e| e.id == "git")
        .unwrap()
        .clone();
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
    assert!(
        !entry.is_parsed(),
        "Empty slot should remain Empty after sweep"
    );
}

#[test]
fn evict_idle_then_get_returns_fresh_arc_with_same_contents() {
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", &minimal_spec("git"));
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    let arc1 = store.get("git").expect("first get must resolve");
    let entry = store
        .entries()
        .iter()
        .find(|e| e.id == "git")
        .unwrap()
        .clone();
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

    let report = store
        .last_sweep()
        .expect("last_sweep must record after a run");
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
    let entry = store
        .entries()
        .iter()
        .find(|e| e.id == "git")
        .unwrap()
        .clone();
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
    let git_entry = store
        .entries()
        .iter()
        .find(|e| e.id == "git")
        .unwrap()
        .clone();
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
    assert_eq!(
        errors.len(),
        1,
        "only the broken entry should report an error"
    );
    assert_eq!(errors[0].id, "broken");
    assert_eq!(
        store.parsed_count(),
        1,
        "git must have been re-parsed by force_load"
    );
}

fn write_n_specs(dir: &std::path::Path, n: usize) {
    for i in 0..n {
        write_spec(
            dir,
            &format!("cmd{i}.json"),
            &minimal_spec(&format!("cmd{i}")),
        );
    }
}

#[test]
fn backstop_evicts_oldest_first() {
    let dir = TempDir::new().unwrap();
    write_n_specs(dir.path(), 5);
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;

    // Load all 5; set distinct last_accessed timestamps.
    let now = SystemTime::now();
    let loaded_arcs: Vec<_> = (0..5)
        .map(|i| {
            let id = format!("cmd{i}");
            store.get(&id).expect("spec must load")
        })
        .collect();
    for i in 0..5 {
        let id = format!("cmd{i}");
        let entry = store.entries().iter().find(|e| e.id == id).unwrap().clone();
        // Deliberately make cmd3 oldest so insertion-order eviction would fail.
        let age_secs = match i {
            3 => 500,
            0 => 400,
            1 => 300,
            2 => 200,
            4 => 100,
            _ => unreachable!(),
        };
        entry.set_last_accessed_for_test(now - Duration::from_secs(age_secs));
    }
    assert_eq!(store.parsed_count(), 5);
    let resident_before = store.estimated_resident_bytes();
    assert!(
        resident_before > 1,
        "fixture specs should have non-zero heap estimate"
    );

    // Cap just below the current estimate forces exactly the oldest entry
    // out: freeing cmd3 is enough to get back under the cap.
    let report = store.evict_idle_at(
        now,
        Duration::MAX,             // TTL phase: no-op
        Some(resident_before - 1), // backstop only
        &empty_keep_warm(),
    );
    assert_eq!(report.evicted_backstop_count, 1);

    // Specifically verify cmd3 (oldest) was evicted before cmd0 (first
    // registered) and cmd4 (newest).
    // `is_parsed()` remains true for Evicted slots by design, so Arc identity
    // is the public observable: cmd3 re-parses to a fresh Arc; cmd0/cmd4 stay warm.
    let cmd3_after = store.get("cmd3").expect("cmd3 must reparse");
    let cmd0_after = store.get("cmd0").expect("cmd0 must remain available");
    let cmd4_after = store.get("cmd4").expect("cmd4 must remain available");
    assert!(
        !Arc::ptr_eq(&loaded_arcs[3], &cmd3_after),
        "oldest entry must reparse after backstop eviction"
    );
    assert!(
        Arc::ptr_eq(&loaded_arcs[0], &cmd0_after),
        "first registered entry should remain resident when it is not oldest"
    );
    assert!(
        Arc::ptr_eq(&loaded_arcs[4], &cmd4_after),
        "newest entry should remain resident when one eviction satisfies cap"
    );
}

#[test]
fn backstop_respects_keep_warm_under_pressure() {
    let dir = TempDir::new().unwrap();
    write_n_specs(dir.path(), 3);
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    let now = SystemTime::now();
    for i in 0..3 {
        let _ = store.get(&format!("cmd{i}"));
    }
    let cmd0_entry = store
        .entries()
        .iter()
        .find(|e| e.id == "cmd0")
        .unwrap()
        .clone();
    cmd0_entry.set_last_accessed_for_test(now - Duration::from_secs(3600));

    let mut keep_warm = HashSet::new();
    keep_warm.insert("cmd0".to_string());
    let _ = store.evict_idle_at(
        now,
        Duration::MAX,
        Some(1), // pathological cap
        &keep_warm,
    );
    // cmd0 must remain Loaded despite being the oldest.
    let cmd0_after = store.entries().iter().find(|e| e.id == "cmd0").unwrap();
    assert!(
        cmd0_after.spec_arc().is_some(),
        "keep_warm entry must be exempt from backstop eviction"
    );
}

#[test]
fn backstop_warns_when_keep_warm_pin_exceeds_cap() {
    // When every Loaded entry is in keep_warm and total > cap, backstop
    // cannot reach the target. The implementation logs a warn once per
    // sweep. This test pins the behaviour without asserting on the log
    // (log-capture machinery lives in lazy_loading.rs); it asserts that
    // the entries remain Loaded and the report's evicted_backstop_count
    // is zero.
    let dir = TempDir::new().unwrap();
    write_n_specs(dir.path(), 3);
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    for i in 0..3 {
        let _ = store.get(&format!("cmd{i}"));
    }
    let mut keep_warm = HashSet::new();
    for i in 0..3 {
        keep_warm.insert(format!("cmd{i}"));
    }
    let report = store.evict_idle_at(
        SystemTime::now(),
        Duration::MAX,
        Some(1), // cap=1 byte forces backstop
        &keep_warm,
    );
    assert_eq!(
        report.evicted_backstop_count, 0,
        "backstop must not evict keep_warm entries even when cap is unreachable"
    );
    assert_eq!(
        report.parsed_count_after, 3,
        "all three entries must remain Loaded"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn sweep_smoke_evicts_idle_after_interval() {
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", &minimal_spec("git"));
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    let store = Arc::new(store);
    let _ = store.get("git");
    let entry = store
        .entries()
        .iter()
        .find(|e| e.id == "git")
        .unwrap()
        .clone();
    entry.set_last_accessed_for_test(SystemTime::now() - Duration::from_secs(3600));

    let cfg = SpecCacheConfig {
        idle_ttl_secs: 1,
        sweep_interval_secs: 1,
        keep_warm: vec![],
        max_resident_mb: 0,
    };
    let _sweep = gc_suggest::spawn_spec_cache_sweep_for_test(Arc::clone(&store), cfg);
    tokio::task::yield_now().await; // let the task consume interval's initial tick
    assert_eq!(
        store.parsed_count(),
        1,
        "initial interval tick must be skipped; first sweep waits one full interval"
    );

    // Advance virtual time past one sweep tick.
    tokio::time::advance(Duration::from_secs(3)).await;
    tokio::task::yield_now().await; // give the sweep task a chance to run

    assert_eq!(
        store.parsed_count(),
        0,
        "sweep task must have evicted the idle entry"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn sweep_smoke_cancels_on_drop() {
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", &minimal_spec("git"));
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    let store = Arc::new(store);
    let weak_store = Arc::downgrade(&store);

    let cfg = SpecCacheConfig {
        idle_ttl_secs: 1,
        sweep_interval_secs: 1,
        keep_warm: vec![],
        max_resident_mb: 0,
    };
    let sweep = gc_suggest::spawn_spec_cache_sweep_for_test(Arc::clone(&store), cfg);
    drop(store);
    drop(sweep);

    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::task::yield_now().await;
    assert!(
        weak_store.upgrade().is_none(),
        "dropping the sweep guard must let the task exit and release its SpecStore Arc"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn sweep_smoke_no_log_spam_when_nothing_eligible() {
    // The sweep loop's tracing::debug! must be gated on
    // "evicted_idle_count > 0 || evicted_backstop_count > 0". A nothing-
    // eligible sweep must not log. We assert the report itself shows
    // zero evictions across N sweep ticks.
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", &minimal_spec("git"));
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    let store = Arc::new(store);
    let _ = store.get("git"); // Loaded, just-now timestamp

    let cfg = SpecCacheConfig {
        idle_ttl_secs: 3600, // 1h — git's just-now timestamp is not idle
        sweep_interval_secs: 1,
        keep_warm: vec![],
        max_resident_mb: 0,
    };
    let _sweep = gc_suggest::spawn_spec_cache_sweep_for_test(Arc::clone(&store), cfg);
    tokio::task::yield_now().await; // let the task consume interval's initial tick

    // Advance past 5 sweep intervals; each is a no-op.
    tokio::time::advance(Duration::from_secs(5)).await;
    tokio::task::yield_now().await;

    let report = store.last_sweep().expect("at least one sweep ran");
    assert_eq!(report.evicted_idle_count, 0);
    assert_eq!(report.evicted_backstop_count, 0);
    assert_eq!(report.parsed_count_after, 1);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn sweep_smoke_does_not_panic_when_sweep_interval_zero() {
    // `tokio::time::interval(Duration::from_secs(0))` panics. The
    // `sweep_interval_secs.max(1)` clamp inside `spawn_spec_cache_sweep`
    // is the only guard for callers that bypass `GhostConfig::normalize`.
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", &minimal_spec("git"));
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    let store = Arc::new(store);

    let cfg = SpecCacheConfig {
        idle_ttl_secs: 1,
        sweep_interval_secs: 0,
        keep_warm: vec![],
        max_resident_mb: 0,
    };
    let _sweep = gc_suggest::spawn_spec_cache_sweep_for_test(Arc::clone(&store), cfg);
    tokio::task::yield_now().await; // let the task consume interval's initial tick

    tokio::time::advance(Duration::from_secs(3)).await;
    tokio::task::yield_now().await;

    assert!(
        store.last_sweep().is_some(),
        "the clamped 1-second interval must allow at least one sweep to complete"
    );
}

#[test]
fn repeated_get_bumps_last_accessed_each_call() {
    // The read-lock fast path inside `parsed_result_arc` must call
    // `bump_last_accessed` on every successful hit, otherwise an actively-
    // used spec could be evicted as if it were idle.
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", &minimal_spec("git"));
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    let _ = store.get("git"); // first parse, populates Loaded
    let entry = store
        .entries()
        .iter()
        .find(|e| e.id == "git")
        .unwrap()
        .clone();

    // Backdate the timestamp; a second get() must bump it back to ~now.
    let now = SystemTime::now();
    entry.set_last_accessed_for_test(now - Duration::from_secs(3600));
    let _ = store.get("git").expect("warm get must resolve");
    assert!(
        entry.last_accessed() >= now - Duration::from_secs(60),
        "warm read-lock fast path must bump last_accessed on every hit"
    );

    // The just-warmed entry must survive a TTL sweep.
    let report = store.evict_idle_at(
        SystemTime::now(),
        Duration::from_secs(60),
        None,
        &empty_keep_warm(),
    );
    assert_eq!(report.evicted_idle_count, 0);
    assert_eq!(store.parsed_count(), 1);
}

#[test]
fn backstop_warn_emits_only_once_across_repeated_sweeps() {
    // `backstop_cap_warned` is one-shot for the lifetime of the SpecStore.
    // Three sweeps under unreachable-cap pressure must produce exactly one
    // warn line, not three.
    let dir = TempDir::new().unwrap();
    write_n_specs(dir.path(), 3);
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    for i in 0..3 {
        let _ = store.get(&format!("cmd{i}"));
    }
    let mut keep_warm = HashSet::new();
    for i in 0..3 {
        keep_warm.insert(format!("cmd{i}"));
    }

    let (captured, _guard) = install_log_capture();
    for _ in 0..3 {
        let _ = store.evict_idle_at(SystemTime::now(), Duration::MAX, Some(1), &keep_warm);
    }

    let logs = captured_logs(&captured);
    assert_eq!(
        logs.matches("backstop unable to reach cap").count(),
        1,
        "warn-once guard must collapse repeated unreachable-cap sweeps to a single log line:\n{logs}"
    );
}

#[test]
fn evict_idle_on_empty_store_reports_zero() {
    // An empty spec dir produces a SpecStore with zero entries. evict_idle
    // must walk it without panicking and report all-zero counters.
    let dir = TempDir::new().unwrap();
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    let report = store.evict_idle_at(
        SystemTime::now(),
        Duration::from_secs(0),
        Some(0), // also exercises the cap=0 path with no entries
        &empty_keep_warm(),
    );
    assert_eq!(report.evicted_idle_count, 0);
    assert_eq!(report.evicted_backstop_count, 0);
    assert_eq!(report.parsed_count_after, 0);
    assert_eq!(report.estimated_resident_bytes_after, 0);
}

#[test]
fn backstop_with_single_entry_evicts_to_zero_under_pressure() {
    // Cap = 0 forces the backstop to evict every Loaded entry. The
    // saturating-sub of `current` against `freed` keeps the loop sound when
    // freed bytes exceed remaining bytes. Post-sweep, the store must
    // re-parse cleanly on the next get.
    let dir = TempDir::new().unwrap();
    write_spec(dir.path(), "git.json", &minimal_spec("git"));
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    let _ = store.get("git").expect("first load must succeed");
    assert_eq!(store.parsed_count(), 1);

    let report = store.evict_idle_at(
        SystemTime::now(),
        Duration::MAX, // disable Phase 1
        Some(0),       // cap=0 forces every Loaded entry out via Phase 2
        &empty_keep_warm(),
    );
    assert_eq!(report.evicted_backstop_count, 1);
    assert_eq!(store.parsed_count(), 0);

    // Post-sweep, the entry must re-parse cleanly through the standard
    // `get` path.
    let arc = store
        .get("git")
        .expect("post-eviction re-parse must succeed");
    assert_eq!(arc.name, "git");
    assert_eq!(store.parsed_count(), 1);
}
