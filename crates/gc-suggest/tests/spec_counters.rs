//! Integration tests for [`SpecStore::counters`] — corpus-wide
//! diagnostics for the ux-9b precursor migration plan.
//!
//! These pin the per-shape contributions to the [`SpecResolutionCounters`]
//! fields so future converter work can extend the migration-future fields
//! (`lowered_to_transforms`, `static_extracted_subprocess`,
//! `token_only_promoted`, `aws_sdk_dispatched`,
//! `native_provider_dispatched`) without re-deriving the requires_js
//! totals.
//!
//! The "supported" classification mirrors the engine's runtime dispatch
//! gate (see [`gc_suggest::specs::is_requires_js_supported`]):
//! `script_function` / `custom` generators must carry both a non-empty
//! `source` AND `self_contained: true`; `post_process` generators must
//! carry an accompanying `script` / `script_template` plus a non-empty
//! `source`. Without this alignment, the counters block would over-report
//! supported generators relative to the legacy raw-JSON walker that the
//! `ghost-complete status --json spec_counts` block already publishes.

use std::fs;

use gc_suggest::SpecStore;
use tempfile::TempDir;

fn write_spec(dir: &std::path::Path, filename: &str, body: &str) {
    fs::write(dir.join(filename), body).unwrap();
}

/// Build a tempdir corpus with:
///   - one static-only spec (no requires_js generators);
///   - one requires_js generator with no `js_runtime` (unsupported shape);
///   - one requires_js generator with a populated `js_runtime` carrying
///     `kind=script_function`, non-empty `source`, AND
///     `self_contained: true` — the canonical supported shape.
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

    // 3. requires_js generator with js_runtime populated AND
    //    `self_contained: true`. Lives under a subcommand's args to also
    //    exercise the recursive walk.
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
                            "source": "ctx => ['a','b']",
                            "self_contained": true
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
        "one requires_js generator carries js_runtime + self_contained metadata"
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

/// `script_function` + `js_runtime` populated + non-empty source +
/// `self_contained: false` (the converter could not prove the JS body
/// closes over no bundler helpers) MUST land in `requires_js_unsupported`,
/// not `requires_js_supported`. The runtime drops these at dispatch
/// (`gc-suggest::specs::collect_generators`) and the diagnostic counters
/// must report the same truthful state — otherwise downstream migration
/// phases would track different supported populations than the engine
/// actually loads.
#[test]
fn script_function_without_self_contained_is_unsupported() {
    let dir = TempDir::new().unwrap();
    write_spec(
        dir.path(),
        "needs-self-contained.json",
        r#"{
            "name": "needs-self-contained",
            "args": [{
                "name": "x",
                "generators": [{
                    "requires_js": true,
                    "js_runtime": {
                        "kind": "script_function",
                        "source": "ctx => ['x']"
                    }
                }]
            }]
        }"#,
    );
    let counters = SpecStore::load_from_dir(dir.path())
        .unwrap()
        .store
        .counters();

    assert_eq!(counters.requires_js_total, 1);
    assert_eq!(
        counters.requires_js_supported, 0,
        "script_function without self_contained must NOT be classified supported"
    );
    assert_eq!(counters.requires_js_unsupported, 1);
}

/// `custom` + `js_runtime` populated + non-empty source +
/// `self_contained: false` mirrors `script_function`: must land in
/// unsupported, not supported.
#[test]
fn custom_without_self_contained_is_unsupported() {
    let dir = TempDir::new().unwrap();
    write_spec(
        dir.path(),
        "needs-self-contained-custom.json",
        r#"{
            "name": "needs-self-contained-custom",
            "args": [{
                "name": "x",
                "generators": [{
                    "requires_js": true,
                    "js_runtime": {
                        "kind": "custom",
                        "source": "async ctx => ['x']"
                    }
                }]
            }]
        }"#,
    );
    let counters = SpecStore::load_from_dir(dir.path())
        .unwrap()
        .store
        .counters();

    assert_eq!(counters.requires_js_total, 1);
    assert_eq!(counters.requires_js_supported, 0);
    assert_eq!(counters.requires_js_unsupported, 1);
}

/// `post_process` does NOT need `self_contained: true`. It DOES need an
/// accompanying `script` (or `script_template`) plus a non-empty source.
#[test]
fn post_process_with_script_is_supported_without_self_contained() {
    let dir = TempDir::new().unwrap();
    write_spec(
        dir.path(),
        "post-process.json",
        r#"{
            "name": "post-process",
            "args": [{
                "name": "x",
                "generators": [{
                    "requires_js": true,
                    "script": ["ls"],
                    "js_runtime": {
                        "kind": "post_process",
                        "source": "out => out.split('\\n')"
                    }
                }]
            }]
        }"#,
    );
    let counters = SpecStore::load_from_dir(dir.path())
        .unwrap()
        .store
        .counters();

    assert_eq!(counters.requires_js_total, 1);
    assert_eq!(
        counters.requires_js_supported, 1,
        "post_process with accompanying script + non-empty source is supported"
    );
    assert_eq!(counters.requires_js_unsupported, 0);
}

/// `post_process` WITHOUT an accompanying script (or script_template) is
/// unsupported — the JS body has no stdout to consume.
#[test]
fn post_process_without_script_is_unsupported() {
    let dir = TempDir::new().unwrap();
    write_spec(
        dir.path(),
        "post-process-no-script.json",
        r#"{
            "name": "post-process-no-script",
            "args": [{
                "name": "x",
                "generators": [{
                    "requires_js": true,
                    "js_runtime": {
                        "kind": "post_process",
                        "source": "out => out.split('\\n')"
                    }
                }]
            }]
        }"#,
    );
    let counters = SpecStore::load_from_dir(dir.path())
        .unwrap()
        .store
        .counters();

    assert_eq!(counters.requires_js_total, 1);
    assert_eq!(counters.requires_js_supported, 0);
    assert_eq!(counters.requires_js_unsupported, 1);
}

/// Empty `js_runtime.source` strings are not dispatchable — there is
/// nothing to evaluate. Both `script_function` and `post_process` must
/// reject this shape.
#[test]
fn empty_source_is_unsupported() {
    let dir = TempDir::new().unwrap();
    write_spec(
        dir.path(),
        "empty-source.json",
        r#"{
            "name": "empty-source",
            "args": [{
                "name": "x",
                "generators": [{
                    "requires_js": true,
                    "js_runtime": {
                        "kind": "script_function",
                        "source": "   ",
                        "self_contained": true
                    }
                }]
            }]
        }"#,
    );
    let counters = SpecStore::load_from_dir(dir.path())
        .unwrap()
        .store
        .counters();

    assert_eq!(counters.requires_js_total, 1);
    assert_eq!(
        counters.requires_js_supported, 0,
        "whitespace-only source has nothing to evaluate"
    );
    assert_eq!(counters.requires_js_unsupported, 1);
}
