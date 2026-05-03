//! Phase 5 (UX-9) end-to-end tests for the `script_function` and
//! `custom` JS dispatch paths.
//!
//! Phase 4 covered `post_process` (stdout-in / suggestions-out). Phase 5
//! adds two new shapes that the engine routes through dedicated dispatch
//! helpers (`run_script_function_dispatch`, `run_custom_dispatch`):
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
use std::sync::Arc;

use gc_buffer::{CommandContext, QuoteState};
use gc_suggest::commands::CommandsProvider;
use gc_suggest::history::HistoryProvider;
use gc_suggest::specs::{GeneratorSpec, JsRuntimeKind, JsRuntimeSpec, SpecStore};
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
    let temp = tempfile::tempdir().expect("tempdir");
    let store = SpecStore::load_from_dir(temp.path())
        .expect("empty dir loads")
        .store;
    let history = HistoryProvider::from_entries(Vec::new());
    let commands = CommandsProvider::from_list(Vec::new());
    SuggestionEngine::with_providers(store, history, commands)
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
        js_runtime: Some(JsRuntimeSpec {
            kind: JsRuntimeKind::ScriptFunction,
            source: source.to_string(),
            input: None,
            timeout_ms: None,
            allow_shell_command: false,
        }),
        corrected_in: None,
        template: None,
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
        js_runtime: Some(JsRuntimeSpec {
            kind: JsRuntimeKind::Custom,
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
async fn phase5_script_function_returns_dynamic_suggestions() {
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
async fn phase5_script_function_descriptor_form() {
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
async fn phase5_script_function_invalid_argv_yields_empty() {
    // Returning a number is invalid — the runtime emits InvalidArgv and
    // the engine surfaces zero suggestions.
    let gen = script_function_generator("(tokens) => 42");
    let engine = make_engine();
    let ctx = make_ctx("phase5-test", Vec::new(), "");
    let results = engine
        .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
        .await
        .expect("dispatch tolerates invalid argv");
    assert!(results.is_empty());
}

#[tokio::test]
async fn phase5_custom_uses_host_runner() {
    // Custom generator runs `printf foo\\nbar` via the host runner
    // binding and turns the lines into suggestions.
    let source = "async (tokens, run, ctx) => { \
        const out = await run(['sh', '-c', 'printf \"foo\\\\nbar\\\\n\"']); \
        return out.split('\\n').filter(Boolean).map(name => ({ name })); \
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
async fn phase5_custom_can_read_cwd_and_tokens() {
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
    assert!(names.contains(&"tokens:phase5-test,sub"), "got: {names:?}",);
}

#[tokio::test]
async fn phase5_custom_unsupported_host_api_logs_diagnostic() {
    // Reach for an unsupported Fig API; the spec catches the throw and
    // surfaces the diagnostic code as a suggestion. Engine returns the
    // resulting suggestion verbatim.
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
}

#[tokio::test]
async fn phase5_custom_kill_switch_disables_dispatch() {
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
async fn phase5_custom_aws_describe_regions_corpus_fixture() {
    // Real corpus fixture: Fig's AWS spec invokes
    // `aws ec2 describe-regions` and JSON-parses the output. We mock
    // the aws binary by inlining the JSON in the JS source, exercising
    // the same JSON-parse → field-extract pattern.
    let source = "async (tokens, run, ctx) => { \
        const aws_json = '{\"Regions\":[{\"RegionName\":\"us-east-1\"},{\"RegionName\":\"eu-west-1\"}]}'; \
        const out = await run(['sh', '-c', 'printf %s ' + JSON.stringify(aws_json)]); \
        const parsed = JSON.parse(out); \
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
async fn phase5_custom_kubectl_get_pods_corpus_fixture() {
    // kubectl-style spec: `kubectl get pods -o name` would produce one
    // line per pod. The custom generator splits and surfaces each one.
    let source = "async (tokens, run, ctx) => { \
        const out = await run(['sh', '-c', 'printf \"pod/web-1\\\\npod/web-2\\\\npod/api-1\\\\n\"']); \
        return out.split('\\n').filter(Boolean).map(name => ({ name })); \
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
async fn phase5_custom_docker_images_corpus_fixture() {
    // docker-style spec: `docker images --format` would emit a list.
    // We exercise the same shape via printf for hermeticity.
    let source = "async (tokens, run, ctx) => { \
        const out = await run(['sh', '-c', 'printf \"alpine:3.18\\\\nubuntu:22.04\\\\n\"']); \
        return out.split('\\n').filter(Boolean).map(name => ({ name })); \
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
async fn phase5_script_function_cargo_workspace_corpus_fixture() {
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
async fn phase5_supported_count_lifts_to_full_corpus() {
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
            js_runtime: Some(JsRuntimeSpec {
                kind: JsRuntimeKind::PostProcess,
                source: "out => out.split('\\n').filter(Boolean).map(n => ({ name: 'pp:' + n }))"
                    .to_string(),
                input: None,
                timeout_ms: None,
                allow_shell_command: false,
            }),
            corrected_in: None,
            template: None,
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
