//! Integration tests for [`SpecStore::counters`] — corpus-wide
//! diagnostics for the ux-9b precursor migration plan.
//!
//! These pin the per-shape contributions to the [`SpecResolutionCounters`]
//! fields so future converter work can extend the migration-future fields
//! (`lowered_to_transforms`, `static_extracted_subprocess`,
//! `token_only_promoted`, `aws_sdk_dispatched`,
//! `native_provider_dispatched`) without re-deriving the requires_js
//! totals.

use std::fs;

use gc_suggest::SpecStore;
use tempfile::TempDir;

fn write_spec(dir: &std::path::Path, filename: &str, body: &str) {
    fs::write(dir.join(filename), body).unwrap();
}

/// Build a tempdir corpus with:
///   - one static-only spec (no requires_js generators);
///   - one requires_js generator with no `js_runtime` (unsupported shape);
///   - one requires_js generator with a populated `js_runtime`
///     (supported shape).
fn fixture_corpus() -> TempDir {
    let dir = TempDir::new().unwrap();

    // 1. Static-only spec: no requires_js generators anywhere. Should not
    //    contribute to any counter.
    write_spec(
        dir.path(),
        "static-cmd.json",
        r#"{
            "name": "static-cmd",
            "subcommands": [{"name": "go"}],
            "options": [{"name": ["--flag"]}]
        }"#,
    );

    // 2. requires_js generator without js_runtime metadata. Walks via
    //    args.generators.
    write_spec(
        dir.path(),
        "unsupported-cmd.json",
        r#"{
            "name": "unsupported-cmd",
            "args": [{
                "name": "thing",
                "generators": [{"requires_js": true, "js_source": "ctx => []"}]
            }]
        }"#,
    );

    // 3. requires_js generator with js_runtime populated. Lives under a
    //    subcommand's args to also exercise the recursive walk.
    write_spec(
        dir.path(),
        "supported-cmd.json",
        r#"{
            "name": "supported-cmd",
            "subcommands": [{
                "name": "deploy",
                "args": [{
                    "name": "target",
                    "generators": [{
                        "requires_js": true,
                        "js_runtime": {
                            "kind": "script_function",
                            "source": "ctx => ['a','b']"
                        }
                    }]
                }]
            }]
        }"#,
    );

    dir
}

#[test]
fn counters_classify_each_known_generator_shape() {
    let dir = fixture_corpus();
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    let counters = store.counters();

    assert_eq!(
        counters.requires_js_total, 2,
        "two requires_js generators in fixture (one supported, one unsupported)"
    );
    assert_eq!(
        counters.requires_js_supported, 1,
        "one requires_js generator carries js_runtime metadata"
    );
    assert_eq!(
        counters.requires_js_unsupported, 1,
        "one requires_js generator omits js_runtime metadata"
    );

    // Migration-future fields stay at 0 in this PR — populated by
    // ux-10/11/12/13/14 respectively.
    assert_eq!(counters.lowered_to_transforms, 0);
    assert_eq!(counters.static_extracted_subprocess, 0);
    assert_eq!(counters.token_only_promoted, 0);
    assert_eq!(counters.aws_sdk_dispatched, 0);
    assert_eq!(counters.native_provider_dispatched, 0);
}

#[test]
fn adding_unsupported_requires_js_generator_increments_unsupported() {
    let dir = fixture_corpus();
    let baseline = SpecStore::load_from_dir(dir.path())
        .unwrap()
        .store
        .counters();

    // Add another spec carrying a requires_js generator without
    // js_runtime metadata. Must increment `requires_js_total` and
    // `requires_js_unsupported` by exactly one.
    write_spec(
        dir.path(),
        "extra-unsupported.json",
        r#"{
            "name": "extra-unsupported",
            "options": [{
                "name": ["--target"],
                "args": {
                    "name": "value",
                    "generators": [{"requires_js": true, "js_source": "ctx => []"}]
                }
            }]
        }"#,
    );

    let after = SpecStore::load_from_dir(dir.path())
        .unwrap()
        .store
        .counters();

    assert_eq!(after.requires_js_total, baseline.requires_js_total + 1);
    assert_eq!(
        after.requires_js_unsupported,
        baseline.requires_js_unsupported + 1,
        "an unsupported requires_js generator must bump only the unsupported counter"
    );
    assert_eq!(
        after.requires_js_supported, baseline.requires_js_supported,
        "supported counter must remain unchanged"
    );
}

#[test]
fn empty_corpus_has_zero_counters() {
    let dir = TempDir::new().unwrap();
    let store = SpecStore::load_from_dir(dir.path()).unwrap().store;
    let counters = store.counters();

    assert_eq!(counters.requires_js_total, 0);
    assert_eq!(counters.requires_js_supported, 0);
    assert_eq!(counters.requires_js_unsupported, 0);
    assert_eq!(counters.lowered_to_transforms, 0);
    assert_eq!(counters.static_extracted_subprocess, 0);
    assert_eq!(counters.token_only_promoted, 0);
    assert_eq!(counters.aws_sdk_dispatched, 0);
    assert_eq!(counters.native_provider_dispatched, 0);
}
