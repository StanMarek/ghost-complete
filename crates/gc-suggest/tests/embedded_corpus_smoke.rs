//! Full-corpus smoke test for the lazy zstd-compressed embedded spec
//! archive. Iterates every embedded filename, decompresses each body,
//! parses the JSON, and times the cumulative cold-load loop.
//!
//! The ≤ 200 ms ceiling is the SPEC acceptance criterion for ux-12b
//! "all 709 specs decompress and parse … cumulative load time < 200 ms
//! cold." That target is for the **release-profile** build — zstd-19
//! decompression is CPU-bound and materially slower under debug builds.
//! The debug-profile budget is relaxed to keep the correctness signal
//! (every spec decompresses and parses) stable on GitHub Actions runners.
//!
//! The correctness assertions (every spec resolves, every JSON parses)
//! always run. The timing-budget assertion fires only when the
//! environment claims to be CI-like (`CI=true` or any non-empty,
//! non-zero value) — a busy developer box would otherwise flake on the
//! 200 ms ceiling without the gate.
//!
//! Run with `--nocapture` to see the cumulative figure:
//!
//! ```sh
//! # Correctness-only on a non-idle box:
//! cargo test --release -p gc-suggest --test embedded_corpus_smoke -- --nocapture
//!
//! # With timing budget enforced (mirrors CI):
//! CI=true cargo test --release -p gc-suggest --test embedded_corpus_smoke -- --nocapture
//! ```

use std::time::Instant;

use gc_suggest::{embedded_filenames, embedded_spec_contents, embedded_spec_count};

#[cfg(debug_assertions)]
const CORPUS_LOAD_BUDGET_MS: u128 = 4_000;
#[cfg(not(debug_assertions))]
const CORPUS_LOAD_BUDGET_MS: u128 = 200;

/// Treat the environment as CI-like when `CI` is set to any non-empty
/// value other than `"0"` / `"false"`. This matches how GitHub Actions,
/// CircleCI, GitLab CI, and most other CI providers expose the env var.
fn is_ci_environment() -> bool {
    match std::env::var("CI") {
        Ok(v) => {
            let trimmed = v.trim();
            !trimmed.is_empty()
                && !trimmed.eq_ignore_ascii_case("0")
                && !trimmed.eq_ignore_ascii_case("false")
        }
        Err(_) => false,
    }
}

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

    // Correctness assertion: always-on. Every spec must resolve and parse.
    assert!(
        parse_failures.is_empty(),
        "{} embedded spec(s) failed to load:\n{}",
        parse_failures.len(),
        parse_failures.join("\n")
    );

    // Timing-budget assertion: CI-only. A busy developer box would
    // otherwise flake on the 200 ms ceiling.
    if is_ci_environment() {
        assert!(
            elapsed.as_millis() <= CORPUS_LOAD_BUDGET_MS,
            "cold corpus decompression + parse took {} ms, exceeds budget of \
             {CORPUS_LOAD_BUDGET_MS} ms",
            elapsed.as_millis()
        );
    } else {
        eprintln!(
            "(perf budget skipped: set CI=true to enforce the {CORPUS_LOAD_BUDGET_MS} ms budget)"
        );
    }
}
