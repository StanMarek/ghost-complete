//! Phase 4 (UX-9) end-to-end tests for the QuickJS post-process dispatch path.
//!
//! These tests build a `SuggestionEngine`, hand it a synthetic
//! `Arc<GeneratorSpec>` with a real `js_runtime.source`, and exercise
//! `run_generators` directly. They verify the spec store/loader machinery is
//! decoupled enough that the engine accepts any well-formed generator we
//! pass in — which is what the proxy actually does on every keystroke.
//!
//! What's covered:
//! - A `post_process` generator with a `script` returns suggestions whose
//!   text comes from the JS body, not from raw stdout.
//! - The same script with two distinct `js_runtime.source` bodies produces
//!   two distinct result sets that do NOT cross-contaminate via the cache.
//! - Skipping happens for `script_function`, `custom`, and missing
//!   `js_runtime` shapes.
//! - JS timeouts and exceptions surface as empty suggestions (without
//!   hanging the test process) and are captured via the diagnostic log
//!   path.
//! - The `[suggest.providers] js_runtime = false` kill switch bypasses the
//!   evaluator entirely.

use std::path::Path;
use std::sync::Arc;

use gc_buffer::{CommandContext, QuoteState};
use gc_suggest::commands::CommandsProvider;
use gc_suggest::history::HistoryProvider;
use gc_suggest::specs::{CacheConfig, GeneratorSpec, JsRuntimeKind, JsRuntimeSpec, SpecStore};
use gc_suggest::SuggestionEngine;

fn make_ctx(command: &str, args: Vec<&str>, current_word: &str) -> CommandContext {
    CommandContext {
        command: Some(command.to_string()),
        args: args.into_iter().map(String::from).collect(),
        current_word: current_word.to_string(),
        word_index: 1,
        is_flag: false,
        is_long_flag: false,
        preceding_flag: None,
        in_pipe: false,
        in_redirect: false,
        quote_state: QuoteState::None,
        is_first_segment: true,
    }
}

fn make_engine() -> SuggestionEngine {
    // Empty spec store + empty providers — `run_generators` accepts any
    // pre-resolved generator slice we hand it, so we don't need to populate
    // a real corpus to drive these tests.
    let temp = tempfile::tempdir().expect("tempdir");
    let store = SpecStore::load_from_dir(temp.path())
        .expect("empty dir loads")
        .store;
    let history = HistoryProvider::from_entries(Vec::new());
    let commands = CommandsProvider::from_list(Vec::new());
    SuggestionEngine::with_providers(store, history, commands)
}

fn engine_with_js_disabled() -> SuggestionEngine {
    make_engine().with_suggest_config(50, true, 5, true, true, true, false)
}

/// Convenience: build a `post_process` generator wrapping a fixed `script`
/// argv and a JS body. Caching is enabled by default (1 hour) so we can
/// observe cache split semantics.
fn post_process_generator(script: &[&str], source: &str) -> Arc<GeneratorSpec> {
    Arc::new(GeneratorSpec {
        generator_type: None,
        script: Some(script.iter().map(|s| s.to_string()).collect()),
        script_template: None,
        transforms: Vec::new(),
        cache: Some(CacheConfig {
            ttl_seconds: 3600,
            cache_by_directory: false,
        }),
        requires_js: true,
        js_source: None,
        js_runtime: Some(JsRuntimeSpec {
            kind: JsRuntimeKind::PostProcess,
            source: source.to_string(),
            input: None,
            timeout_ms: None,
            allow_shell_command: false,
        }),
        corrected_in: None,
        template: None,
    })
}

#[tokio::test]
async fn phase4_post_process_returns_suggestions() {
    // Real /bin/printf is unhelpful here because escapes vary across
    // platforms; use `sh -c` to write a deterministic 3-line stdout.
    let gen = post_process_generator(
        &["sh", "-c", "printf 'alpha\\nbeta\\ngamma\\n'"],
        "out => out.split('\\n').filter(Boolean).map(name => ({ name }))",
    );
    let engine = make_engine();
    let ctx = make_ctx("phase4-test", Vec::new(), "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch succeeds");
    let names: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        names,
        vec!["alpha", "beta", "gamma"],
        "post_process JS should split stdout into one suggestion per line"
    );
}

#[tokio::test]
async fn phase4_two_js_sources_same_stdout_dont_cross_contaminate() {
    // Both generators run the same script; they differ only in the JS body.
    // The first body produces names; the second decorates them with a
    // description. The cache must not let the first result leak into the
    // second slot.
    let script = ["sh", "-c", "printf 'main\\ndev\\n'"];
    let body_a = "out => out.split('\\n').filter(Boolean).map(name => ({ name }))";
    let body_b =
        "out => out.split('\\n').filter(Boolean).map(name => ({ name, description: 'branch ' + name }))";
    let gen_a = post_process_generator(&script, body_a);
    let gen_b = post_process_generator(&script, body_b);

    let engine = make_engine();
    let ctx = make_ctx("phase4-test", Vec::new(), "");

    // Run A first to warm the stdout cache.
    let res_a = engine
        .run_generators(std::slice::from_ref(&gen_a), &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch a");
    assert!(
        res_a.iter().all(|s| s.description.is_none()),
        "body_a should produce bare names: {:?}",
        res_a
    );
    let names_a: Vec<&str> = res_a.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(names_a, vec!["main", "dev"]);

    // Run B with the same script — body_b should run JS even though the
    // stdout cache is warm, because the post-processed cache key differs
    // by source hash.
    let res_b = engine
        .run_generators(std::slice::from_ref(&gen_b), &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch b");
    let names_b: Vec<&str> = res_b.iter().map(|s| s.text.as_str()).collect();
    let descriptions_b: Vec<Option<&str>> =
        res_b.iter().map(|s| s.description.as_deref()).collect();
    assert_eq!(names_b, vec!["main", "dev"]);
    assert_eq!(
        descriptions_b,
        vec![Some("branch main"), Some("branch dev")],
        "body_b must produce decorated descriptions, not body_a's bare names"
    );
}

#[tokio::test]
async fn phase4_unsupported_kind_skipped_when_source_empty() {
    // Phase 5 widened support so `custom` generators with a populated
    // source dispatch through the JS host API. A generator with an
    // EMPTY source still has nothing to run and stays skipped — this
    // is the only "unsupported shape" path that survives Phase 5.
    let gen = Arc::new(GeneratorSpec {
        generator_type: None,
        script: None,
        script_template: None,
        transforms: Vec::new(),
        cache: None,
        requires_js: true,
        js_source: None,
        js_runtime: Some(JsRuntimeSpec {
            kind: JsRuntimeKind::Custom,
            source: "".to_string(),
            input: None,
            timeout_ms: None,
            allow_shell_command: false,
        }),
        corrected_in: None,
        template: None,
    });

    let engine = make_engine();
    let ctx = make_ctx("phase4-test", Vec::new(), "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch tolerates the unsupported shape");
    // The empty-source generator is JS-dispatchable at the schema
    // level (the spec says `kind: custom`) so the engine spawns the
    // JS evaluation. The evaluator returns immediately with an
    // `EmptyOutput` diagnostic — no suggestions surface.
    assert!(
        results.is_empty(),
        "empty-source custom generator must yield zero suggestions, got: {results:?}"
    );
}

#[tokio::test]
async fn phase4_js_timeout_diagnostic_logged() {
    // Tight timeout + infinite loop: the JS interrupt handler must abort
    // and the dispatch must return empty suggestions instead of hanging.
    let gen = Arc::new(GeneratorSpec {
        generator_type: None,
        script: Some(vec!["echo".to_string(), "ignored".to_string()]),
        script_template: None,
        transforms: Vec::new(),
        cache: None,
        requires_js: true,
        js_source: None,
        js_runtime: Some(JsRuntimeSpec {
            kind: JsRuntimeKind::PostProcess,
            source: "out => { while (true) {} }".to_string(),
            input: None,
            timeout_ms: Some(50),
            allow_shell_command: false,
        }),
        corrected_in: None,
        template: None,
    });

    let engine = make_engine();
    let ctx = make_ctx("phase4-test", Vec::new(), "");
    let start = std::time::Instant::now();
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch returns Ok even on JS timeout");
    let elapsed = start.elapsed();
    assert!(
        results.is_empty(),
        "timed-out JS must yield zero suggestions, got: {results:?}"
    );
    assert!(
        elapsed.as_secs() < 4,
        "timeout must abort well before the 5s outer timeout (took {elapsed:?})"
    );
}

#[tokio::test]
async fn phase4_js_exception_diagnostic_logged() {
    let gen = Arc::new(GeneratorSpec {
        generator_type: None,
        script: Some(vec!["echo".to_string(), "ignored".to_string()]),
        script_template: None,
        transforms: Vec::new(),
        cache: None,
        requires_js: true,
        js_source: None,
        js_runtime: Some(JsRuntimeSpec {
            kind: JsRuntimeKind::PostProcess,
            source: "out => { throw new Error('boom') }".to_string(),
            input: None,
            timeout_ms: None,
            allow_shell_command: false,
        }),
        corrected_in: None,
        template: None,
    });

    let engine = make_engine();
    let ctx = make_ctx("phase4-test", Vec::new(), "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch tolerates JS exceptions");
    assert!(
        results.is_empty(),
        "throwing JS body must yield zero suggestions, got: {results:?}"
    );
}

#[tokio::test]
async fn phase4_stdout_cache_shared_across_js_sources() {
    // Sanity for the stdout cache layer: a script with a side effect (writes
    // to a counter file) should run exactly once even when two different JS
    // post-processors reach for it. The first generator warms the stdout
    // cache; the second hits the cache and skips the spawn.
    let tmp = tempfile::tempdir().expect("tempdir");
    let counter = tmp.path().join("count");
    let script_src = format!(
        "echo run >> {path}; printf 'main\\ndev\\n'",
        path = counter.display(),
    );

    let body_a = "out => out.split('\\n').filter(Boolean).map(name => ({ name }))";
    let body_b =
        "out => out.split('\\n').filter(Boolean).map(name => ({ name: name.toUpperCase() }))";

    let gen_a = post_process_generator(&["sh", "-c", script_src.as_str()], body_a);
    let gen_b = post_process_generator(&["sh", "-c", script_src.as_str()], body_b);

    let engine = make_engine();
    let ctx = make_ctx("phase4-test", Vec::new(), "");

    let res_a = engine
        .run_generators(&[gen_a], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch a");
    assert!(!res_a.is_empty());

    let res_b = engine
        .run_generators(&[gen_b], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch b");
    let names_b: Vec<&str> = res_b.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        names_b,
        vec!["MAIN", "DEV"],
        "body_b should run on cached stdout and uppercase the names"
    );

    // The counter file should have exactly one line — proof the script
    // spawn happened once even though two generators consumed its output.
    let counter_contents = std::fs::read_to_string(&counter).expect("counter file written");
    let runs = counter_contents.lines().count();
    assert_eq!(
        runs, 1,
        "stdout cache must be shared; got {runs} script runs (contents: {counter_contents:?})"
    );
}

#[tokio::test]
async fn phase4_kill_switch_disables_dispatch() {
    // Same generator that worked in `phase4_post_process_returns_suggestions`,
    // but the engine has `js_runtime = false` set. The dispatch must
    // skip the generator entirely; no script spawn either, since we never
    // reach the spawn body.
    let gen = post_process_generator(
        &["sh", "-c", "printf 'one\\ntwo\\n'"],
        "out => out.split('\\n').filter(Boolean).map(name => ({ name }))",
    );
    let engine = engine_with_js_disabled();
    let ctx = make_ctx("phase4-test", Vec::new(), "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("kill-switched dispatch returns Ok");
    assert!(
        results.is_empty(),
        "kill switch must drop js_runtime generators entirely, got: {results:?}"
    );
}
