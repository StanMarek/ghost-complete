//! Integration tests for the lazy zstd-compressed embedded spec archive.
//!
//! These tests exercise the public API as an external crate would —
//! they intentionally avoid reaching into private build-script internals
//! so a behavioural regression surfaces here regardless of how
//! `embedded.rs` evolves internally.
//!
//! The five specs covered (`git`, `cargo`, `kubectl`, `npm`, `docker`)
//! are stable load-bearing entries of the embedded corpus; if any of
//! them disappear, the corpus restoration plan is the right place to
//! fix it, not this test.

use gc_suggest::{embedded_filenames, embedded_spec_contents, embedded_spec_count};

const REPRESENTATIVE_SPECS: &[&str] = &[
    "git.json",
    "cargo.json",
    "kubectl.json",
    "npm.json",
    "docker.json",
];

#[test]
fn count_and_filenames_agree() {
    // Sanity floor: the corpus should always have at least our five
    // representatives. A regression here means the archive header is
    // empty or truncated.
    assert!(
        embedded_spec_count() >= REPRESENTATIVE_SPECS.len(),
        "embedded corpus has fewer specs ({}) than the representative \
         set requires ({})",
        embedded_spec_count(),
        REPRESENTATIVE_SPECS.len()
    );
    assert_eq!(
        embedded_filenames().len(),
        embedded_spec_count(),
        "embedded_filenames() and embedded_spec_count() must agree"
    );
}

#[test]
fn representative_specs_decompress_and_parse() {
    for filename in REPRESENTATIVE_SPECS {
        let contents = embedded_spec_contents(filename)
            .unwrap_or_else(|| panic!("expected {filename} to be in the embedded corpus"));
        assert!(
            !contents.is_empty(),
            "{filename} decompressed to an empty body"
        );

        let parsed: serde_json::Value = serde_json::from_str(contents)
            .unwrap_or_else(|e| panic!("{filename} failed to parse: {e}"));

        // Every Fig-compatible spec is an object with a top-level `name`.
        assert!(
            parsed.is_object(),
            "{filename} parsed to a non-object value"
        );
        assert!(
            parsed.get("name").and_then(|v| v.as_str()).is_some(),
            "{filename} is missing a top-level `name` field"
        );
    }
}

#[test]
fn cache_returns_identical_pointer_on_second_lookup() {
    // The lazy decompression layer Box::leaks the decoded JSON into a
    // `&'static str` and caches the pointer. parse_entry_source relies
    // on this for `Cow::Borrowed` to remain valid for the process
    // lifetime — a non-stable pointer would mean every parse round-trip
    // leaks fresh memory and any pointer-comparison invariants in the
    // SpecStore would silently break.
    for filename in REPRESENTATIVE_SPECS {
        let first = embedded_spec_contents(filename)
            .unwrap_or_else(|| panic!("expected {filename} on first lookup"));
        let second = embedded_spec_contents(filename)
            .unwrap_or_else(|| panic!("expected {filename} on second lookup"));

        let p1: *const str = first;
        let p2: *const str = second;
        assert_eq!(
            p1, p2,
            "embedded_spec_contents({filename}) returned a different pointer on the second lookup — \
             cache is not pointer-stable"
        );
    }
}

#[test]
fn missing_filename_returns_none() {
    assert!(
        embedded_spec_contents("definitely-not-a-real-spec.json").is_none(),
        "lookup for a non-existent filename must return None"
    );
}
