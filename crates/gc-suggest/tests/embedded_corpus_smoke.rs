//! Full-corpus smoke test for the lazy zstd-compressed embedded spec
//! archive. Iterates every embedded filename, decompresses each body,
//! parses the JSON, and times the cumulative cold-load loop.
//!
//! The ≤ 200 ms ceiling is the SPEC acceptance criterion for ux-12b
//! "all 709 specs decompress and parse … cumulative load time < 200 ms
//! cold." That target is for the **release-profile** build — zstd-19
//! decompression is CPU-bound and ~6x slower under debug builds, so
//! the test relaxes the budget by 10x in debug mode to keep the
//! correctness signal (every spec decompresses and parses) usable
//! during local iteration without losing the perf signal in CI release
//! builds. Run with `--nocapture` to see the actual figure:
//!
//! ```sh
//! cargo test --release -p gc-suggest --test embedded_corpus_smoke -- --nocapture
//! ```

use std::time::Instant;

use gc_suggest::{embedded_filenames, embedded_spec_contents, embedded_spec_count};

#[cfg(debug_assertions)]
const CORPUS_LOAD_BUDGET_MS: u128 = 2_000;
#[cfg(not(debug_assertions))]
const CORPUS_LOAD_BUDGET_MS: u128 = 200;

#[test]
fn every_embedded_spec_decompresses_and_parses_within_budget() {
    let start = Instant::now();

    let count = embedded_spec_count();
    let filenames = embedded_filenames();
    assert_eq!(
        filenames.len(),
        count,
        "embedded_filenames() len must match embedded_spec_count()"
    );
    assert!(
        count > 0,
        "embedded corpus must have at least one spec; got {count}"
    );

    let mut parse_failures: Vec<String> = Vec::new();
    let mut total_bytes: usize = 0;

    for filename in filenames {
        let contents = match embedded_spec_contents(filename) {
            Some(c) => c,
            None => {
                parse_failures.push(format!("{filename}: missing from archive"));
                continue;
            }
        };
        total_bytes += contents.len();
        if let Err(e) = serde_json::from_str::<serde_json::Value>(contents) {
            parse_failures.push(format!("{filename}: parse failed: {e}"));
        }
    }

    let elapsed = start.elapsed();
    eprintln!(
        "embedded_corpus_smoke: decompressed + parsed {count} specs in {} ms ({} MB JSON)",
        elapsed.as_millis(),
        total_bytes / (1024 * 1024)
    );

    assert!(
        parse_failures.is_empty(),
        "{} embedded spec(s) failed to load:\n{}",
        parse_failures.len(),
        parse_failures.join("\n")
    );
    assert!(
        elapsed.as_millis() <= CORPUS_LOAD_BUDGET_MS,
        "cold corpus decompression + parse took {} ms, exceeds budget of {CORPUS_LOAD_BUDGET_MS} ms",
        elapsed.as_millis()
    );
}
