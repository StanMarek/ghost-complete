//! Memory regression test for the lazy spec-loading contract.
//!
//! Pre-v0.12.4 the spec loader eagerly parsed every embedded spec into
//! `Arc<CompletionSpec>`. The AWS spec alone (~36 MB minified, ~17 K
//! subcommands, ~116 K descriptions) ballooned the daemon's physical
//! footprint to ~333 MB on first load. v0.12.4 defers `serde_json::from_str`
//! to first `SpecEntry::spec()` call, so a daemon that loads the corpus but
//! never resolves a particular spec keeps the embedded payload as immutable
//! `&'static str` slices in `.rodata` — no heap allocation.
//!
//! This test pins the new ceiling. It runs as its own integration-test
//! binary so the measurement isn't polluted by other tests in the same
//! process. Each `tests/*.rs` file gets a fresh subprocess from the cargo
//! test runner, so the high-water-mark RSS we sample here reflects only
//! the load + the runtime's initial allocations.

use gc_suggest::SpecStore;

/// Maximum resident-set size since process start, in bytes.
///
/// Reads `getrusage(RUSAGE_SELF).ru_maxrss`. The field is monotonic — it
/// never decreases — so we can sample it before and after the load and
/// observe at most an UPPER BOUND on what the load contributed. If the
/// lazy contract is intact, `load_with_embedded` allocates a few KB of
/// `Arc<SpecEntry>` headers and the alias hash table; no spec body is
/// parsed, no `Arc<CompletionSpec>` is materialised.
///
/// Returns bytes on every supported platform: macOS reports bytes
/// directly, Linux reports kilobytes (we multiply through). Other Unix
/// flavours follow whichever convention their kernel chose; we only
/// strictly support macOS + Linux for this test and the ceiling is
/// generous enough to absorb minor reporting deltas.
#[cfg(unix)]
fn max_rss_bytes() -> u64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    assert_eq!(ret, 0, "getrusage(RUSAGE_SELF) failed");
    #[allow(clippy::useless_conversion)]
    let raw = u64::try_from(usage.ru_maxrss).unwrap_or(0);
    if cfg!(target_os = "macos") {
        raw
    } else {
        raw * 1024
    }
}

const MB: u64 = 1024 * 1024;

/// `load_with_embedded` registers the entire embedded corpus without
/// parsing a single spec. The high-water-mark RSS delta after the call
/// must stay well below the pre-fix 333 MB regression. The 50 MB ceiling
/// here gives generous headroom — the actual delta on the author's
/// machine measures ~5 MB — but stays an order of magnitude below the
/// regression we're guarding against.
#[cfg(unix)]
#[test]
fn lazy_load_with_embedded_stays_under_50mb_high_water_mark() {
    let before = max_rss_bytes();
    let result = SpecStore::load_with_embedded(&[]).expect("embedded corpus must load");
    // Hold the store across the measurement so the compiler can't elide it.
    let store = result.store;
    let after = max_rss_bytes();

    let delta = after.saturating_sub(before);
    let delta_mb = delta / MB;

    // 50 MB is the ceiling. Pre-fix the delta was ~333 MB. The current
    // measured delta is single-digit MB. If this test starts failing,
    // it usually means a caller upstream of `load_with_embedded`
    // accidentally re-introduced eager parsing — a `for entry in store.iter()`
    // somewhere in startup, or a `SpecEntry::spec()` call inside the
    // registration loop.
    assert!(
        delta_mb < 50,
        "lazy load grew max RSS by {delta_mb} MB (before={before}B, after={after}B); \
         expected < 50 MB. Look for an upstream caller that force-loads specs at startup."
    );

    // Sanity check: the store is non-empty. A no-op that loaded zero
    // entries would also pass the memory ceiling, so we pin the load
    // succeeded too.
    assert!(
        !store.entries().is_empty(),
        "embedded corpus must register entries"
    );
    // And nothing must have been parsed at this point — that's the lazy
    // contract this whole regression hangs on.
    let parsed_count = store.entries().iter().filter(|e| e.is_parsed()).count();
    assert_eq!(
        parsed_count, 0,
        "lazy contract violated: {parsed_count} entries parsed at load time"
    );
}
