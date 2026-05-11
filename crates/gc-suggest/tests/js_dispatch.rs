//! End-to-end tests for the `script_function` and `custom` JS dispatch
//! paths.
//!
//! Two shapes route through dedicated dispatch helpers
//! (`run_script_function_dispatch`, `run_custom_dispatch`):
//!
//! - `script_function`: JS evaluates first to derive argv, the engine
//!   then runs the argv as a script and applies the optional
//!   transform pipeline.
//! - `custom`: JS evaluates with a host shell-runner binding and
//!   produces suggestions directly.
//!
//! These tests synthesise a `Vec<Arc<GeneratorSpec>>` and pass it to
//! `SuggestionEngine::run_generators` — the same hot path the proxy
//! takes on every keystroke.

use std::path::Path;
use std::sync::{Arc, Mutex};

use gc_buffer::{CommandContext, QuoteState};
use gc_suggest::commands::CommandsProvider;
use gc_suggest::history::HistoryProvider;
use gc_suggest::specs::{GeneratorSpec, JsRuntimeKind, JsRuntimeSpec, SpecStore};
use gc_suggest::SuggestionEngine;
use tracing_subscriber::fmt::MakeWriter;

/// Per-thread tracing capture writer (mirror of the helper in
/// js_post_process_dispatch.rs — the integration-test crate doesn't share a
/// common module, so we duplicate the small writer here).
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
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    (captured, guard)
}

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
    let temp = tempfile::tempdir().expect("tempdir");
    let store = SpecStore::load_from_dir(temp.path())
        .expect("empty dir loads")
        .store;
    let history = HistoryProvider::from_entries(Vec::new());
    let commands = CommandsProvider::from_list(Vec::new());
    SuggestionEngine::with_providers(store, history, commands)
}

/// Build an engine wrapped around a spec written to a fresh temp dir.
/// Returns the `TempDir` along with the engine so the directory
/// outlives the engine — the lazy spec loader keeps a `SpecSource`
/// path back into the temp dir and parses on first use, so dropping
/// the temp dir before the engine would invalidate every lookup.
fn engine_from_spec_json(filename: &str, spec_json: &str) -> (SuggestionEngine, tempfile::TempDir) {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join(filename), spec_json).expect("write spec fixture");
    let store = SpecStore::load_from_dir(temp.path())
        .expect("fixture spec loads")
        .store;
    let history = HistoryProvider::from_entries(Vec::new());
    let commands = CommandsProvider::from_list(Vec::new());
    (
        SuggestionEngine::with_providers(store, history, commands),
        temp,
    )
}

fn script_function_generator(source: &str) -> Arc<GeneratorSpec> {
    Arc::new(GeneratorSpec {
        generator_type: None,
        script: None,
        script_template: None,
        transforms: Vec::new(),
        cache: None,
        requires_js: true,
        js_source: None,
        js_runtime: Some(Arc::new(JsRuntimeSpec {
            kind: JsRuntimeKind::ScriptFunction,
            source: source.to_string(),
            self_contained: true,
            timeout_ms: None,
            allow_shell_command: false,
        })),
        corrected_in: None,
        template: None,
        params: std::collections::BTreeMap::new(),
    })
}

fn custom_generator(source: &str) -> Arc<GeneratorSpec> {
    Arc::new(GeneratorSpec {
        generator_type: None,
        script: None,
        script_template: None,
        transforms: Vec::new(),
        cache: None,
        requires_js: true,
        js_source: None,
        js_runtime: Some(Arc::new(JsRuntimeSpec {
            kind: JsRuntimeKind::Custom,
            source: source.to_string(),
            self_contained: true,
            timeout_ms: None,
            allow_shell_command: false,
        })),
        corrected_in: None,
        template: None,
        params: std::collections::BTreeMap::new(),
    })
}

fn token_only_generator(source: &str, self_contained: bool) -> Arc<GeneratorSpec> {
    Arc::new(GeneratorSpec {
        generator_type: None,
        script: None,
        script_template: None,
        transforms: Vec::new(),
        cache: None,
        requires_js: true,
        js_source: None,
        js_runtime: Some(Arc::new(JsRuntimeSpec {
            kind: JsRuntimeKind::TokenOnly,
            source: source.to_string(),
            self_contained,
            timeout_ms: None,
            allow_shell_command: false,
        })),
        corrected_in: None,
        template: None,
        params: std::collections::BTreeMap::new(),
    })
}

fn token_only_generator_with_timeout(source: &str, timeout_ms: u64) -> Arc<GeneratorSpec> {
    Arc::new(GeneratorSpec {
        generator_type: None,
        script: None,
        script_template: None,
        transforms: Vec::new(),
        cache: None,
        requires_js: true,
        js_source: None,
        js_runtime: Some(Arc::new(JsRuntimeSpec {
            kind: JsRuntimeKind::TokenOnly,
            source: source.to_string(),
            self_contained: false,
            timeout_ms: Some(timeout_ms),
            allow_shell_command: false,
        })),
        corrected_in: None,
        template: None,
        params: std::collections::BTreeMap::new(),
    })
}

fn custom_generator_with_shell_string(source: &str) -> Arc<GeneratorSpec> {
    Arc::new(GeneratorSpec {
        generator_type: None,
        script: None,
        script_template: None,
        transforms: Vec::new(),
        cache: None,
        requires_js: true,
        js_source: None,
        js_runtime: Some(Arc::new(JsRuntimeSpec {
            kind: JsRuntimeKind::Custom,
            source: source.to_string(),
            self_contained: true,
            timeout_ms: None,
            allow_shell_command: true,
        })),
        corrected_in: None,
        template: None,
        params: std::collections::BTreeMap::new(),
    })
}

fn cached_custom_generator(source: &str) -> Arc<GeneratorSpec> {
    Arc::new(GeneratorSpec {
        generator_type: None,
        script: None,
        script_template: None,
        transforms: Vec::new(),
        cache: Some(gc_suggest::specs::CacheConfig {
            ttl_seconds: 3600,
            cache_by_directory: false,
        }),
        requires_js: true,
        js_source: None,
        js_runtime: Some(Arc::new(JsRuntimeSpec {
            kind: JsRuntimeKind::Custom,
            source: source.to_string(),
            self_contained: true,
            timeout_ms: None,
            allow_shell_command: false,
        })),
        corrected_in: None,
        template: None,
        params: std::collections::BTreeMap::new(),
    })
}

fn cached_token_only_generator(source: &str) -> Arc<GeneratorSpec> {
    Arc::new(GeneratorSpec {
        generator_type: None,
        script: None,
        script_template: None,
        transforms: Vec::new(),
        cache: Some(gc_suggest::specs::CacheConfig {
            ttl_seconds: 3600,
            cache_by_directory: false,
        }),
        requires_js: true,
        js_source: None,
        js_runtime: Some(Arc::new(JsRuntimeSpec {
            kind: JsRuntimeKind::TokenOnly,
            source: source.to_string(),
            self_contained: false,
            timeout_ms: None,
            allow_shell_command: false,
        })),
        corrected_in: None,
        template: None,
        params: std::collections::BTreeMap::new(),
    })
}

#[tokio::test]
async fn script_function_returns_dynamic_suggestions() {
    // JS produces argv `["sh", "-c", "printf alpha\nbeta\n"]`.
    // The engine then spawns the argv and the default transform
    // pipeline (split_lines, filter_empty) yields two suggestions.
    let gen =
        script_function_generator("(tokens, ctx) => ['sh', '-c', 'printf \"alpha\\nbeta\\n\"']");
    let engine = make_engine();
    let ctx = make_ctx("phase5-test", Vec::new(), "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch");
    let names: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(names, vec!["alpha", "beta"]);
}

#[tokio::test]
async fn script_function_descriptor_form() {
    // Structured descriptor `{ command, args }` resolves the same way.
    let gen = script_function_generator(
        "(tokens, ctx) => ({ command: 'sh', args: ['-c', 'printf \"one\\nthree\\n\"'] })",
    );
    let engine = make_engine();
    let ctx = make_ctx("phase5-test", Vec::new(), "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch");
    let names: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(names, vec!["one", "three"]);
}

#[tokio::test]
async fn script_function_can_read_fig_context_aliases() {
    let home = std::env::var("HOME").expect("HOME should be set in test environment");
    let cwd = tempfile::tempdir().expect("tempdir");
    let expected_cwd = cwd.path().to_string_lossy().into_owned();
    let gen = script_function_generator(
        "(tokens, ctx) => ({ \
            command: 'printf', \
            args: [ \
                '%s\\n%s\\n', \
                'cwd:' + ctx.currentWorkingDirectory, \
                'home:' + ctx.environmentVariables.HOME, \
            ], \
        })",
    );
    let engine = make_engine();
    let ctx = make_ctx("phase5-test", Vec::new(), "");
    let results = engine
        .run_generators(&[gen], &ctx, cwd.path(), 5_000)
        .await
        .expect("dispatch");
    let names: Vec<String> = results.iter().map(|s| s.text.clone()).collect();
    assert_eq!(
        names,
        vec![format!("cwd:{expected_cwd}"), format!("home:{home}")]
    );
}

#[tokio::test]
async fn script_function_invalid_argv_yields_empty() {
    // Returning a number is invalid — the runtime emits InvalidArgv and
    // the engine surfaces zero suggestions. We also assert the structured
    // diagnostic warn fires (`code = "invalid_argv"`) so a future refactor
    // that demotes it to trace! or drops it entirely cannot silently erase
    // the operator-visible signal that doctor / log scrapers depend on.
    let (captured, _guard) = install_log_capture();

    let gen = script_function_generator("(tokens) => 42");
    let engine = make_engine();
    let ctx = make_ctx("phase5-test", Vec::new(), "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch tolerates invalid argv");
    assert!(results.is_empty());

    let logs =
        String::from_utf8_lossy(&captured.lock().expect("capture buffer poisoned")).into_owned();
    assert!(
        logs.contains("code=\"invalid_argv\""),
        "expected a structured warn with `code = \"invalid_argv\"` (the lowercase \
         tag from JsDiagnosticCode::tag) in captured logs:\n{logs}"
    );
    assert!(
        logs.contains("script_function returned invalid argv"),
        "expected the human-readable invalid-argv message in captured logs:\n{logs}"
    );
    assert!(
        logs.contains("generator_id=phase5-test#0"),
        "expected the generator_id field with the test fixture's name in \
         captured logs:\n{logs}"
    );
}

#[tokio::test]
async fn custom_uses_host_runner() {
    // Custom generator runs `printf foo\\nbar` via the host runner
    // binding and turns the lines into suggestions.
    let source = "async (tokens, run, ctx) => { \
        const { stdout } = await run(['sh', '-c', 'printf \"foo\\\\nbar\\\\n\"']); \
        return stdout.split('\\n').filter(Boolean).map(name => ({ name })); \
    }";
    let gen = custom_generator(source);
    let engine = make_engine();
    let ctx = make_ctx("phase5-test", Vec::new(), "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch");
    let names: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(names, vec!["foo", "bar"]);
}

#[tokio::test]
async fn custom_can_read_cwd_and_tokens() {
    // Custom that reflects the host context as suggestions, no shell.
    // Uses an empty current_word so the engine returns the raw pool
    // without fuzzy-ranking (which would filter our literal-prefix
    // suggestions away as non-matches).
    let source = "async (tokens, run, ctx) => [ \
        { name: 'cwd:' + ctx.cwd }, \
        { name: 'tokens:' + tokens.join(',') }, \
    ]";
    let gen = custom_generator(source);
    let engine = make_engine();
    let ctx = make_ctx("phase5-test", vec!["sub"], "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/phase5-cwd"), 5_000)
        .await
        .expect("dispatch");
    let names: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
    assert!(names.contains(&"cwd:/phase5-cwd"), "got: {names:?}");
    assert!(names.contains(&"tokens:phase5-test,sub,"), "got: {names:?}",);
}

#[tokio::test]
async fn custom_tokens_include_non_empty_current_word() {
    let source = "async (tokens, run, ctx) => [{ \
        name: 'last:' + tokens[tokens.length - 1], \
        description: 'current:' + ctx.currentToken + ',previous:' + ctx.previousToken, \
    }]";
    let gen = custom_generator(source);
    let engine = make_engine();
    let ctx = make_ctx("npm", vec!["install"], "rea");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch");
    let hit = results
        .iter()
        .find(|s| s.text == "last:rea")
        .expect("custom JS should see the live current word in tokens");
    assert_eq!(
        hit.description.as_deref(),
        Some("current:rea,previous:install")
    );
}

#[tokio::test]
async fn token_only_returns_tokens() {
    let gen = token_only_generator("tokens.map(name => ({ name }))", false);
    let engine = make_engine();
    let ctx = make_ctx("kubectl", vec!["get"], "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch");
    let names: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(names, vec!["kubectl", "get"]);
}

#[tokio::test]
async fn token_only_filters_by_previous_token() {
    let source = "previousToken === 'get' \
        ? ['pods', 'services'].map(name => ({ name })) \
        : ['apply', 'delete'].map(name => ({ name }))";
    let gen = token_only_generator(source, false);
    let engine = make_engine();
    let ctx = make_ctx("kubectl", vec!["get"], "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch");
    let names: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(names, vec!["pods", "services"]);
}

#[tokio::test]
async fn token_only_runtime_error_returns_no_suggestions() {
    let (captured, _guard) = install_log_capture();
    let gen = token_only_generator("(() => { throw new Error('boom'); })()", false);
    let engine = make_engine();
    let ctx = make_ctx("kubectl", vec!["get"], "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch tolerates runtime errors");
    assert!(results.is_empty());

    let logs =
        String::from_utf8_lossy(&captured.lock().expect("capture buffer poisoned")).into_owned();
    assert!(
        logs.contains("code=\"exception\""),
        "expected token_only runtime exception diagnostic in logs:\n{logs}"
    );
    assert!(
        logs.contains("generator_id=kubectl#0#token_only"),
        "expected token_only generator_id in logs:\n{logs}"
    );
}

#[tokio::test]
async fn token_only_repeated_timeouts_are_demoted() {
    let (captured, _guard) = install_log_capture();
    let gen = token_only_generator_with_timeout("() => { while (true) {} }", 5);
    let engine = make_engine();
    let ctx = make_ctx("kubectl", vec!["get"], "");

    for _ in 0..3 {
        let results = engine
            .run_generators(&[Arc::clone(&gen)], &ctx, Path::new("/tmp"), 5_000)
            .await
            .expect("dispatch tolerates repeated token_only timeouts");
        assert!(results.is_empty());
    }

    let logs =
        String::from_utf8_lossy(&captured.lock().expect("capture buffer poisoned")).into_owned();
    assert_eq!(
        logs.matches("js_runtime: evaluation timed out").count(),
        2,
        "third dispatch should skip the demoted generator instead of evaluating JS:\n{logs}"
    );
    assert!(
        logs.contains("token_only generator demoted after repeated timeouts"),
        "expected token_only demotion log:\n{logs}"
    );
    assert!(
        logs.contains("token_only generator is demoted after repeated timeouts"),
        "expected token_only skip log:\n{logs}"
    );
}

/// A real success between two timeout windows resets the consecutive-failure
/// counter — three timeouts split by a working dispatch must NOT demote the
/// generator because only the post-success run-of-two should count.
#[tokio::test]
async fn token_only_success_resets_consecutive_timeout_count() {
    let (captured, _guard) = install_log_capture();
    let slow = token_only_generator_with_timeout("() => { while (true) {} }", 5);
    let fast = token_only_generator("tokens.map(name => ({ name }))", false);
    let engine = make_engine();
    let ctx = make_ctx("kubectl", vec!["get"], "");

    // 1st: timeout — counter at 1.
    let r1 = engine
        .run_generators(&[Arc::clone(&slow)], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("first dispatch tolerates timeout");
    assert!(r1.is_empty());

    // 2nd: real Suggestions payload — counter resets to 0.
    let r2 = engine
        .run_generators(&[Arc::clone(&fast)], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("recovery dispatch produces suggestions");
    assert!(!r2.is_empty(), "fast generator must produce suggestions");

    // 3rd and 4th: two more timeouts. Counter should run from 0 → 1 → 2,
    // hitting the demote-after threshold ONLY on the fourth dispatch.
    for _ in 0..2 {
        let r = engine
            .run_generators(&[Arc::clone(&slow)], &ctx, Path::new("/tmp"), 5_000)
            .await
            .expect("dispatch tolerates timeouts");
        assert!(r.is_empty());
    }

    let logs =
        String::from_utf8_lossy(&captured.lock().expect("capture buffer poisoned")).into_owned();
    assert_eq!(
        logs.matches("js_runtime: evaluation timed out").count(),
        3,
        "all three timeout dispatches should evaluate JS — the success must \
         have reset the consecutive-failure counter:\n{logs}"
    );
    assert!(
        !logs.contains("token_only generator is demoted after repeated timeouts"),
        "demotion warning must NOT fire when a success interleaves the timeouts:\n{logs}"
    );
}

/// Exception-only failures must also count toward demotion — `record_success`
/// only resets on a real Suggestions payload, not on every non-timeout
/// outcome. Three back-to-back exceptions should demote the generator.
#[tokio::test]
async fn token_only_repeated_exceptions_are_demoted() {
    let (captured, _guard) = install_log_capture();
    let gen = token_only_generator("(() => { throw new Error('boom'); })()", false);
    let engine = make_engine();
    let ctx = make_ctx("kubectl", vec!["get"], "");

    for _ in 0..3 {
        let results = engine
            .run_generators(&[Arc::clone(&gen)], &ctx, Path::new("/tmp"), 5_000)
            .await
            .expect("dispatch tolerates repeated exceptions");
        assert!(results.is_empty());
    }

    let logs =
        String::from_utf8_lossy(&captured.lock().expect("capture buffer poisoned")).into_owned();
    assert!(
        logs.contains("token_only generator demoted after repeated runtime failures"),
        "exception-only loop must demote the generator:\n{logs}"
    );
    assert!(
        logs.contains("token_only generator is demoted after repeated timeouts"),
        "demoted generator must subsequently be skipped:\n{logs}"
    );
}

#[tokio::test]
async fn token_only_with_self_contained_false_is_scheduled_from_spec() {
    let (engine, _spec_dir) = engine_from_spec_json(
        "phase12-token-only.json",
        r#"{
            "name": "phase12-token-only",
            "args": [{
                "name": "resource",
                "generators": [{
                    "requires_js": true,
                    "js_runtime": {
                        "kind": "token_only",
                        "self_contained": false,
                        "source": "tokens.map(name => ({ name }))"
                    }
                }]
            }]
        }"#,
    );
    let ctx = make_ctx("phase12-token-only", Vec::new(), "");

    let result = engine
        .suggest_sync(&ctx, Path::new("/tmp"), "phase12-token-only ")
        .expect("suggest_sync");

    assert_eq!(
        result.script_generators.len(),
        1,
        "TokenOnly must be scheduled even when self_contained=false"
    );

    let dynamic = engine
        .run_generators(&result.script_generators, &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch");
    let names: Vec<&str> = dynamic.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(names, vec!["phase12-token-only"]);
}

#[tokio::test]
async fn script_function_tokens_include_empty_cursor_slot_after_space() {
    let gen = script_function_generator(
        "(tokens, ctx) => ['printf', '%s\\n%s\\n%s\\n', \
            'last:' + tokens[tokens.length - 1], \
            'current:' + ctx.currentToken, \
            'previous:' + ctx.previousToken]",
    );
    let engine = make_engine();
    let ctx = make_ctx("npm", vec!["install"], "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch");
    let names: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(names, vec!["last:", "current:", "previous:install"]);
}

#[tokio::test]
async fn custom_cache_key_includes_current_and_previous_token() {
    let source = "async (tokens, run, ctx) => [{ \
        name: 'current:' + ctx.currentToken + ',previous:' + ctx.previousToken, \
    }]";
    let gen = cached_custom_generator(source);
    let engine = make_engine();

    let ctx_rea = make_ctx("npm", vec!["install"], "rea");
    let first = engine
        .run_generators(
            std::slice::from_ref(&gen),
            &ctx_rea,
            Path::new("/tmp"),
            5_000,
        )
        .await
        .expect("first dispatch");
    assert_eq!(first[0].text, "current:rea,previous:install");

    let ctx_pre = make_ctx("npm", vec!["install"], "pre");
    let second = engine
        .run_generators(&[gen], &ctx_pre, Path::new("/tmp"), 5_000)
        .await
        .expect("second dispatch");
    assert_eq!(
        second[0].text, "current:pre,previous:install",
        "custom cache must not reuse results from a different live prefix"
    );
}

#[tokio::test]
async fn token_only_cache_key_includes_current_and_previous_token() {
    let source = "(tokens, ctx) => [{ \
        name: 'current:' + ctx.currentToken + ',previous:' + ctx.previousToken, \
    }]";
    let gen = cached_token_only_generator(source);
    let engine = make_engine();

    let ctx_rea = make_ctx("npm", vec!["install"], "rea");
    let first = engine
        .run_generators(
            std::slice::from_ref(&gen),
            &ctx_rea,
            Path::new("/tmp"),
            5_000,
        )
        .await
        .expect("first dispatch");
    assert_eq!(first[0].text, "current:rea,previous:install");

    let ctx_pre = make_ctx("npm", vec!["install"], "pre");
    let second = engine
        .run_generators(&[gen], &ctx_pre, Path::new("/tmp"), 5_000)
        .await
        .expect("second dispatch");
    assert_eq!(
        second[0].text, "current:pre,previous:install",
        "token_only cache must not reuse results from a different live prefix"
    );
}

#[tokio::test]
async fn custom_can_read_fig_context_aliases() {
    let home = std::env::var("HOME").expect("HOME should be set in test environment");
    let source = "async (tokens, run, ctx) => [ \
        { name: 'cwd:' + ctx.currentWorkingDirectory }, \
        { name: 'home:' + ctx.environmentVariables.HOME }, \
    ]";
    let gen = custom_generator(source);
    let engine = make_engine();
    let ctx = make_ctx("phase5-test", Vec::new(), "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/phase5-cwd"), 5_000)
        .await
        .expect("dispatch");
    let names: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
    assert!(names.contains(&"cwd:/phase5-cwd"), "got: {names:?}");
    assert!(
        names.contains(&format!("home:{home}").as_str()),
        "got: {names:?}",
    );
}

#[tokio::test]
async fn suggest_sync_schedules_requires_js_custom_generator() {
    let home = std::env::var("HOME").expect("HOME should be set in test environment");
    let (engine, _spec_dir) = engine_from_spec_json(
        "phase5-sync-custom.json",
        r#"{
            "name": "phase5-sync-custom",
            "args": [{
                "name": "target",
                "generators": [{
                    "requires_js": true,
                    "js_runtime": {
                        "kind": "custom",
                        "self_contained": true,
                        "source": "async (tokens, run, ctx) => [{ name: 'cwd:' + ctx.currentWorkingDirectory }, { name: 'home:' + ctx.environmentVariables.HOME }]"
                    }
                }]
            }]
        }"#,
    );
    let ctx = make_ctx("phase5-sync-custom", Vec::new(), "");

    let result = engine
        .suggest_sync(&ctx, Path::new("/phase5-cwd"), "phase5-sync-custom ")
        .expect("suggest_sync");

    assert_eq!(
        result.script_generators.len(),
        1,
        "suggest_sync must schedule supported requires_js custom generators"
    );
    assert!(result.script_generators[0].requires_js);

    let dynamic = engine
        .run_generators(
            &result.script_generators,
            &ctx,
            Path::new("/phase5-cwd"),
            5_000,
        )
        .await
        .expect("dispatch");
    let names: Vec<&str> = dynamic.iter().map(|s| s.text.as_str()).collect();
    assert!(names.contains(&"cwd:/phase5-cwd"), "got: {names:?}");
    assert!(
        names.contains(&format!("home:{home}").as_str()),
        "got: {names:?}",
    );
}

#[tokio::test]
async fn custom_unsupported_host_api_logs_diagnostic() {
    // Reach for an unsupported Fig API; the spec catches the throw and
    // surfaces the diagnostic code as a suggestion. Engine returns the
    // resulting suggestion verbatim AND emits a structured warn so
    // `doctor` can surface the misconfiguration. We assert both signals —
    // the suggestion (user-visible behaviour) and the log line
    // (operator-visible behaviour).
    let (captured, _guard) = install_log_capture();
    let source = "async (tokens, run, ctx) => { \
        try { \
            fig.fs.readFile('/etc/hosts'); \
        } catch (e) { \
            return [{ name: 'caught:' + e.code }]; \
        } \
        return [{ name: 'no-error' }]; \
    }";
    let gen = custom_generator(source);
    let engine = make_engine();
    let ctx = make_ctx("phase5-test", Vec::new(), "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch");
    let names: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
    assert!(
        names.contains(&"caught:UnsupportedHostApi"),
        "expected unsupported API diagnostic, got: {names:?}",
    );

    let logs =
        String::from_utf8_lossy(&captured.lock().expect("capture buffer poisoned")).into_owned();
    assert!(
        logs.contains("code=\"unsupported_host_api\""),
        "expected a structured warn with `code = \"unsupported_host_api\"` \
         (the lowercase tag from JsDiagnosticCode::tag) in captured logs:\n{logs}"
    );
    assert!(
        logs.contains("generator_id=phase5-test#0"),
        "expected the generator_id field with the test fixture's name in \
         captured logs:\n{logs}"
    );
}

#[tokio::test]
async fn custom_kill_switch_disables_dispatch() {
    let source = "async () => [{ name: 'should-not-run' }]";
    let gen = custom_generator(source);
    let engine = make_engine().with_suggest_config(50, true, 5, true, true, true, false);
    let ctx = make_ctx("phase5-test", Vec::new(), "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch");
    assert!(
        results.is_empty(),
        "kill switch must drop custom generators entirely"
    );
}

#[tokio::test]
async fn custom_aws_describe_regions_corpus_fixture() {
    // Real corpus fixture: Fig's AWS spec invokes
    // `aws ec2 describe-regions` and JSON-parses the output. We mock
    // the aws binary by inlining the JSON in the JS source, exercising
    // the same JSON-parse → field-extract pattern.
    let source = "async (tokens, run, ctx) => { \
        const aws_json = '{\"Regions\":[{\"RegionName\":\"us-east-1\"},{\"RegionName\":\"eu-west-1\"}]}'; \
        const { stdout } = await run(['sh', '-c', 'printf %s ' + JSON.stringify(aws_json)]); \
        const parsed = JSON.parse(stdout); \
        return parsed.Regions.map(r => ({ name: r.RegionName })); \
    }";
    let gen = custom_generator(source);
    let engine = make_engine();
    let ctx = make_ctx("aws", vec!["ec2"], "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch");
    let names: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(names, vec!["us-east-1", "eu-west-1"]);
}

#[tokio::test]
async fn custom_kubectl_get_pods_corpus_fixture() {
    // kubectl-style spec: `kubectl get pods -o name` would produce one
    // line per pod. The custom generator splits and surfaces each one.
    let source = "async (tokens, run, ctx) => { \
        const { stdout } = await run(['sh', '-c', 'printf \"pod/web-1\\\\npod/web-2\\\\npod/api-1\\\\n\"']); \
        return stdout.split('\\n').filter(Boolean).map(name => ({ name })); \
    }";
    let gen = custom_generator(source);
    let engine = make_engine();
    let ctx = make_ctx("kubectl", vec!["get", "pods"], "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch");
    let names: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(names, vec!["pod/web-1", "pod/web-2", "pod/api-1"]);
}

#[tokio::test]
async fn custom_docker_images_corpus_fixture() {
    // docker-style spec: `docker images --format` would emit a list.
    // We exercise the same shape via printf for hermeticity.
    let source = "async (tokens, run, ctx) => { \
        const { stdout } = await run(['sh', '-c', 'printf \"alpine:3.18\\\\nubuntu:22.04\\\\n\"']); \
        return stdout.split('\\n').filter(Boolean).map(name => ({ name })); \
    }";
    let gen = custom_generator(source);
    let engine = make_engine();
    let ctx = make_ctx("docker", vec!["run"], "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch");
    let names: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(names, vec!["alpine:3.18", "ubuntu:22.04"]);
}

#[tokio::test]
async fn script_function_cargo_workspace_corpus_fixture() {
    // cargo-style: a script_function that derives `cargo --color=never
    // metadata --format-version=1 --no-deps` argv at runtime, then the
    // engine spawns it. We exercise the JS argv path with a printf
    // stub.
    let source = "(tokens, ctx) => ['sh', '-c', 'printf \"crate-a\\\\ncrate-b\\\\n\"']";
    let gen = script_function_generator(source);
    let engine = make_engine();
    let ctx = make_ctx("cargo", vec!["build"], "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch");
    let names: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(names, vec!["crate-a", "crate-b"]);
}

#[tokio::test]
async fn supported_count_lifts_to_full_corpus() {
    // Smoke test: every kind dispatches successfully through
    // run_generators when a non-empty source is supplied. The
    // ghost-complete status walker mirrors this same classification —
    // see is_post_process_supported in ghost-complete::status.
    let gens: Vec<Arc<GeneratorSpec>> = vec![
        Arc::new(GeneratorSpec {
            generator_type: None,
            script: Some(vec!["sh".into(), "-c".into(), "printf 'pp\\n'".into()]),
            script_template: None,
            transforms: Vec::new(),
            cache: None,
            requires_js: true,
            js_source: None,
            js_runtime: Some(Arc::new(JsRuntimeSpec {
                kind: JsRuntimeKind::PostProcess,
                source: "out => out.split('\\n').filter(Boolean).map(n => ({ name: 'pp:' + n }))"
                    .to_string(),
                self_contained: true,
                timeout_ms: None,
                allow_shell_command: false,
            })),
            corrected_in: None,
            template: None,
            params: std::collections::BTreeMap::new(),
        }),
        script_function_generator("(t) => ['sh', '-c', 'printf \"sf\\\\n\"']"),
        custom_generator("async () => [{ name: 'cu' }]"),
    ];
    let engine = make_engine();
    let ctx = make_ctx("phase5", Vec::new(), "");
    let results = engine
        .run_generators(&gens, &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch");
    let names: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
    assert!(names.iter().any(|n| n.starts_with("pp:")), "got: {names:?}");
    assert!(names.contains(&"sf"), "got: {names:?}");
    assert!(names.contains(&"cu"), "got: {names:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_allow_shell_command_dispatches_via_shlex() {
    // `executeShellCommand("printf foo")` is the string form. The runner
    // routes through `shlex::split` and dispatches as argv. The runtime
    // requires `allow_shell_command: true` to honour the string form;
    // without that flag the call would surface a `ShellCommandStringDenied`
    // diagnostic instead.
    let source = "async (tokens, run, ctx) => { \
        const { stdout } = await run('printf foo'); \
        return [{ name: 'shlex:' + stdout }]; \
    }";
    let gen = custom_generator_with_shell_string(source);
    let engine = make_engine();
    let ctx = make_ctx("phase5-test", Vec::new(), "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch");
    let names: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(names, vec!["shlex:foo"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_allow_shell_command_shlex_failure_surfaces_diagnostic() {
    // An unmatched quote can't be parsed by shlex; the runner returns
    // `ShellRunError::ArgvParse`, which the host translates to a
    // `ShellCommandFailed` diagnostic. The spec catches the JS exception
    // and surfaces its `code` field as a suggestion.
    let source = "async (tokens, run, ctx) => { \
        try { \
            await run('echo \"unmatched'); \
        } catch (e) { \
            return [{ name: 'caught:' + e.code }]; \
        } \
        return [{ name: 'no-error' }]; \
    }";
    let gen = custom_generator_with_shell_string(source);
    let engine = make_engine();
    let ctx = make_ctx("phase5-test", Vec::new(), "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch");
    let names: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
    assert!(
        names.contains(&"caught:ShellCommandFailed"),
        "expected ShellCommandFailed diagnostic, got: {names:?}"
    );
}
