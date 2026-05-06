//! Mimic the daemon's idle-state memory: load `SpecStore::load_with_embedded`
//! the way the proxy's `SuggestionEngine::new` does, then park so an
//! external profiler (vmmap, leaks, etc.) can sample the process.
//!
//! Pre-v0.12.4 the eager parse path ballooned this to ~333 MB on first
//! call. Post-fix the steady-state should sit in single-digit MB until
//! the user types something that triggers a real lookup.
//!
//! Run with `cargo run --release --example idle_load -- <seconds>`.
//! Default is 10 seconds.

use std::time::Duration;

use gc_suggest::SpecStore;

fn main() {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let result = SpecStore::load_with_embedded(&[]).expect("embedded corpus must load");
    let store = result.store;

    println!(
        "registered {} entries; sleeping {} seconds — sample with vmmap -summary {}",
        store.entries().len(),
        secs,
        std::process::id()
    );
    std::thread::sleep(Duration::from_secs(secs));

    // Hold the store across the sleep so the optimizer can't drop it.
    drop(store);
}
