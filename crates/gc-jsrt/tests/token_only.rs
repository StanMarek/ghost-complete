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
