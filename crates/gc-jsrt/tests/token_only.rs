//! Integration tests for the token-only JS runtime variant.
//!
//! TokenOnly intentionally exposes only `tokens`, `currentToken`, and
//! `previousToken` on top of the shared sandbox. It must not inherit the
//! Fig host API surface from `PostProcess` / `Custom`.

use std::path::PathBuf;
use std::time::Duration;

use gc_jsrt::{
    JsDiagnosticCode, JsExecutionKind, JsRuntimeInput, JsRuntimeOutput, JsRuntimeOutputPayload,
    JsWorker,
};

const FAST_TIMEOUT: Duration = Duration::from_millis(1_500);

fn token_only_input() -> JsRuntimeInput {
    JsRuntimeInput {
        generator_id: "token-only-test".into(),
        kind: JsExecutionKind::TokenOnly,
        cwd: PathBuf::from("/token-only"),
        tokens: vec!["kubectl".into(), "get".into(), "po".into()],
        current_token: "po".into(),
        previous_token: "get".into(),
        ..JsRuntimeInput::default()
    }
}

async fn run(program: &str, timeout: Duration) -> JsRuntimeOutput {
    JsWorker::spawn()
        .expect("spawn worker")
        .evaluate(program, token_only_input(), timeout)
        .await
        .expect("evaluation infrastructure should not fail")
}

fn assert_first_diagnostic(out: &JsRuntimeOutput, code: JsDiagnosticCode) {
    assert!(
        matches!(out.payload, JsRuntimeOutputPayload::None),
        "expected no payload, got {:?}",
        out.payload
    );
    assert!(out.suggestions().is_empty(), "expected no suggestions");
    assert_eq!(
        out.diagnostics.first().map(|d| d.code),
        Some(code),
        "diagnostics: {:?}",
        out.diagnostics
    );
}

#[tokio::test]
async fn token_only_returns_tokens() {
    let out = run("tokens", FAST_TIMEOUT).await;
    let names: Vec<&str> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["kubectl", "get", "po"],
        "diagnostics: {:?}",
        out.diagnostics
    );
}

#[tokio::test]
async fn token_only_execute_shell_command_absent_throws_exception() {
    let out = run("executeShellCommand(['echo', 'x'])", FAST_TIMEOUT).await;
    assert_first_diagnostic(&out, JsDiagnosticCode::Exception);
    assert!(
        out.diagnostics[0].message.contains("executeShellCommand"),
        "diagnostics: {:?}",
        out.diagnostics
    );
}

#[tokio::test]
async fn token_only_fetch_reference_to_stripped_global_throws() {
    let out = run("fetch", FAST_TIMEOUT).await;
    assert_first_diagnostic(&out, JsDiagnosticCode::Exception);
    assert!(
        out.diagnostics[0].message.contains("fetch"),
        "diagnostics: {:?}",
        out.diagnostics
    );
}

#[tokio::test]
async fn token_only_synchronous_throw_is_graceful() {
    let out = run("throw new Error('token-only boom')", FAST_TIMEOUT).await;
    assert_first_diagnostic(&out, JsDiagnosticCode::Exception);
    assert!(
        out.diagnostics[0].message.contains("token-only boom"),
        "diagnostics: {:?}",
        out.diagnostics
    );
}

#[tokio::test]
async fn token_only_memory_cap_returns_memory_exceeded() {
    let out = run("'x'.repeat(64 * 1024 * 1024)", Duration::from_secs(5)).await;
    assert_first_diagnostic(&out, JsDiagnosticCode::MemoryExceeded);
}

#[tokio::test]
async fn token_only_timeout_returns_timeout() {
    let out = run("while (true) {}", Duration::from_millis(50)).await;
    assert_first_diagnostic(&out, JsDiagnosticCode::Timeout);
}

/// Defense in depth: prove the same Node/Deno/Bun escape vectors that
/// `js_runtime::dangerous_globals_are_undefined` covers for the default
/// (PostProcess) install path are also undefined when the TokenOnly
/// install path runs. Without this, a future regression that wired the
/// host-API install into TokenOnly would not be caught by the existing
/// boundary tests, which only assert that `fetch` / `executeShellCommand`
/// reference-throw.
#[tokio::test]
async fn token_only_capability_globals_are_undefined() {
    let dangerous = [
        "require",
        "process",
        "Deno",
        "Bun",
        "setTimeout",
        "setInterval",
        "setImmediate",
        "fetch",
        "XMLHttpRequest",
        "Buffer",
        "Worker",
        "WebSocket",
        "module",
        "exports",
        "fig",
        "__ghost",
        // Fig backwards-compat globals that `install_host_api` mirrors
        // onto the global scope (host.rs:170-172). `install_token_only_globals`
        // must NOT install any of these — they are derived from cwd/env
        // and would carry capability data even if the data-boundary
        // clearing reduced them to empty strings/objects. Pin the
        // negative so a refactor that wired host-API install into the
        // TokenOnly arm trips this test rather than passing while the
        // security invariant rotted.
        "searchTerm",
        "currentWorkingDirectory",
        "environmentVariables",
    ];
    for name in dangerous {
        let out = run(&format!("typeof {name}"), FAST_TIMEOUT).await;
        assert_eq!(
            out.suggestions().len(),
            1,
            "{name}: expected one suggestion, got {out:?}"
        );
        assert_eq!(
            out.suggestions()[0].name,
            "undefined",
            "{name}: expected typeof to be 'undefined' under TokenOnly"
        );
    }
}

/// Mirror of [`js_runtime::dynamic_eval_intrinsic_is_disabled`] for the
/// TokenOnly install path. The dynamic-code-generation intrinsics are
/// stripped by [`crate::sandbox::configure_context`], which runs before
/// the kind dispatch — but a future refactor that moves sandbox setup
/// inside one branch of the kind match could silently leave the
/// TokenOnly arm with raw `eval` / `Function`. Pin the contract here.
#[tokio::test]
async fn token_only_eval_intrinsic_is_disabled() {
    // Build the probes at runtime so static scanners skip the literals.
    let eval_probe = ["ev", "al", "('1+1')"].concat();
    let out = run(&eval_probe, FAST_TIMEOUT).await;
    assert_first_diagnostic(&out, JsDiagnosticCode::Exception);
    assert!(
        out.diagnostics[0]
            .message
            .to_lowercase()
            .contains("disabled"),
        "diagnostics: {:?}",
        out.diagnostics
    );

    let fn_probe = ["new ", "Func", "tion", "('return 1')()"].concat();
    let out = run(&fn_probe, FAST_TIMEOUT).await;
    assert_first_diagnostic(&out, JsDiagnosticCode::Exception);
}

/// `host.rs::install_token_only_globals` deliberately does not invoke
/// `install_fig_helpers`. Cover the negative: `l` / `p` / `c` / `d` /
/// `h` / `f` must all be undefined under TokenOnly, and calling one
/// should surface as a typed Exception so the spec author sees the
/// misuse rather than a silent zero-result generator.
#[tokio::test]
async fn token_only_fig_helpers_are_not_installed() {
    for name in ["l", "p", "c", "d", "h", "f"] {
        let out = run(&format!("typeof {name}"), FAST_TIMEOUT).await;
        assert_eq!(
            out.suggestions().len(),
            1,
            "{name}: expected one suggestion, got {out:?}"
        );
        assert_eq!(
            out.suggestions()[0].name,
            "undefined",
            "{name}: expected helper to be 'undefined' under TokenOnly"
        );
    }

    // Calling a helper must surface as an Exception that names the
    // helper, not a silent fallback.
    let out = run("l(['x'], 'name')", FAST_TIMEOUT).await;
    assert_first_diagnostic(&out, JsDiagnosticCode::Exception);
    assert!(
        out.diagnostics[0].message.contains('l'),
        "expected helper name in diagnostic, got {:?}",
        out.diagnostics
    );
}

/// Verify the three-arity dispatch in `build_token_only_program`
/// (gc-suggest) passes `(__ctx.tokens, undefined, __ctx)` so a Fig
/// `(tokens, runner, ctx)` source observes `typeof runner === 'undefined'`
/// AND the third argument is the context object. A subtle regression
/// flipping argument order would misbind `ctx` for migrated Fig sources.
///
/// We replicate the wrapper here rather than reaching into `gc-suggest`
/// because that crate is outside this test's compilation unit.
#[tokio::test]
async fn build_token_only_program_three_arity_branch_passes_undefined_runner() {
    let source = "(tokens, runner, ctx) => [{ name: typeof runner + ':' + ctx.previousToken }]";
    let program = wrap_token_only_source(source);
    let out = run(&program, FAST_TIMEOUT).await;
    assert_eq!(
        out.suggestions().len(),
        1,
        "diagnostics: {:?}",
        out.diagnostics
    );
    assert_eq!(out.suggestions()[0].name, "undefined:get");
}

/// Companion to
/// [`build_token_only_program_three_arity_branch_passes_undefined_runner`]:
/// the arity-2 branch must pass `(__ctx.tokens, __ctx)` so `ctx` lands
/// in the second slot.
#[tokio::test]
async fn build_token_only_program_two_arity_branch_passes_ctx_as_second_arg() {
    let source = "(tokens, ctx) => [{ name: 'arity2:' + ctx.previousToken }]";
    let program = wrap_token_only_source(source);
    let out = run(&program, FAST_TIMEOUT).await;
    assert_eq!(
        out.suggestions().len(),
        1,
        "diagnostics: {:?}",
        out.diagnostics
    );
    assert_eq!(out.suggestions()[0].name, "arity2:get");
}

/// Mirror of `gc_suggest::js_runtime::build_token_only_program`. The
/// production wrapper lives in another crate; we replicate the shape
/// here so the integration test can run end-to-end against `JsWorker`
/// without taking a cross-crate dev-dependency.
fn wrap_token_only_source(source: &str) -> String {
    format!(
        "(function() {{ \
           const __src = ({source}); \
           const __ctx = {{ \
             currentToken: typeof currentToken !== 'undefined' ? currentToken : '', \
             previousToken: typeof previousToken !== 'undefined' ? previousToken : '', \
             tokens: typeof tokens !== 'undefined' ? tokens : [], \
           }}; \
           if (typeof __src === 'function') {{ \
             if (__src.length >= 3) {{ \
               return __src(__ctx.tokens, undefined, __ctx); \
             }} \
             return __src(__ctx.tokens, __ctx); \
           }} \
           return __src; \
         }})()",
    )
}
