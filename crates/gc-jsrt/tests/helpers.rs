//! Integration tests for the Fig-compatible single-letter helper preamble
//! installed in the QuickJS sandbox before every job.
//!
//! Background: Fig's `@withfig/autocomplete` bundler emits sub-spec
//! modules with helper functions (typed as `ListOutputGenerator`,
//! `PathOutputGenerator`, etc.) minified down to single letters
//! (`l`, `p`, `c`, `d`, `h`, `f`). The post_process bodies the converter
//! preserved reference those letters by name. Before ux-10a, the QuickJS
//! sandbox did not define them, so the bodies threw `ReferenceError` and
//! the engine silently emitted zero suggestions.
//!
//! These tests pin down the helper semantics derived from the AWS
//! corpus: each is a pure JSON walker over a `JSON.stringify`'d stdout
//! payload. None of them call `executeShellCommand`, `fetch`, or any
//! other host binding — that is enforced indirectly by the
//! `unsupported_host_api_diagnostic` test, which verifies that helper
//! evaluation produces no UnsupportedHostApi diagnostic.

use std::time::Duration;

use gc_jsrt::{JsRuntimeInput, JsWorker};

const FAST_TIMEOUT: Duration = Duration::from_millis(1_500);

fn empty_input() -> JsRuntimeInput {
    JsRuntimeInput {
        generator_id: "test".into(),
        ..Default::default()
    }
}

async fn run(worker: &JsWorker, program: &str) -> gc_jsrt::JsRuntimeOutput {
    worker
        .evaluate(program, empty_input(), FAST_TIMEOUT)
        .await
        .expect("evaluation infrastructure should not fail")
}

/// Drive the helper via a representative post_process body shape. The
/// program is what `build_post_process_program` would emit at runtime.
fn post_process_program(body: &str, stdout: &str) -> String {
    let stdout_lit = serde_json::Value::String(stdout.into()).to_string();
    format!("(({body})({stdout_lit}))")
}

// --- l: 3-arg list extractor ----------------------------------------------

#[tokio::test]
async fn helper_l_extracts_named_field_from_array() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let stdout = r#"{"Roles":[{"RoleName":"admin"},{"RoleName":"viewer"}]}"#;
    let body = r#"function(t){return l(t,"Roles","RoleName")}"#;
    let out = run(&worker, &post_process_program(body, stdout)).await;
    let names: Vec<_> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["admin", "viewer"],
        "helper `l` should return one suggestion per element, projected by name field; diagnostics: {:?}",
        out.diagnostics
    );
}

// --- p: behaves identically to `l` in the corpus --------------------------

#[tokio::test]
async fn helper_p_matches_l_semantics() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let stdout =
        r#"{"CertificateSummaryList":[{"CertificateArn":"arn:1"},{"CertificateArn":"arn:2"}]}"#;
    let body = r#"e=>p(e,"CertificateSummaryList","CertificateArn")"#;
    let out = run(&worker, &post_process_program(body, stdout)).await;
    let names: Vec<_> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["arn:1", "arn:2"],
        "diagnostics: {:?}",
        out.diagnostics
    );
}

// --- c, d, h: same shape ---------------------------------------------------

#[tokio::test]
async fn helper_c_three_arg_form() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let stdout = r#"{"MetricAlarms":[{"AlarmName":"high-cpu"},{"AlarmName":"low-mem"}]}"#;
    let body = r#"t=>c(t,"MetricAlarms","AlarmName")"#;
    let out = run(&worker, &post_process_program(body, stdout)).await;
    let names: Vec<_> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["high-cpu", "low-mem"],
        "diagnostics: {:?}",
        out.diagnostics
    );
}

#[tokio::test]
async fn helper_c_two_arg_form_returns_array_elements_as_strings() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let stdout = r#"{"taskArns":["arn:aws:ecs:t1","arn:aws:ecs:t2"]}"#;
    let body = r#"t=>c(t,"taskArns")"#;
    let out = run(&worker, &post_process_program(body, stdout)).await;
    let names: Vec<_> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["arn:aws:ecs:t1", "arn:aws:ecs:t2"],
        "diagnostics: {:?}",
        out.diagnostics
    );
}

#[tokio::test]
async fn helper_d_three_arg_form() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let stdout = r#"{"apps":[{"appId":"app-1"},{"appId":"app-2"}]}"#;
    let body = r#"t=>d(t,"apps","appId")"#;
    let out = run(&worker, &post_process_program(body, stdout)).await;
    let names: Vec<_> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["app-1", "app-2"],
        "diagnostics: {:?}",
        out.diagnostics
    );
}

#[tokio::test]
async fn helper_h_three_arg_form() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let stdout = r#"{"Roles":[{"RoleName":"admin"}]}"#;
    let body = r#"t=>h(t,"Roles","RoleName")"#;
    let out = run(&worker, &post_process_program(body, stdout)).await;
    let names: Vec<_> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["admin"], "diagnostics: {:?}", out.diagnostics);
}

#[tokio::test]
async fn helper_h_two_arg_form() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let stdout = r#"{"clusters":["cluster-a","cluster-b"]}"#;
    let body = r#"t=>h(t,"clusters")"#;
    let out = run(&worker, &post_process_program(body, stdout)).await;
    let names: Vec<_> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["cluster-a", "cluster-b"],
        "diagnostics: {:?}",
        out.diagnostics
    );
}

// --- f: filter IAM roles by AssumeRolePolicyDocument Principal.Service ----

/// Tiny percent-encoder for the bytes AWS's API actually escapes in
/// AssumeRolePolicyDocument. The full RFC 3986 spec is overkill — the
/// document is JSON, so we only see `{}":,/ ` plus alphanumerics.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[tokio::test]
async fn helper_f_filters_iam_roles_by_principal_service() {
    let worker = JsWorker::spawn().expect("spawn worker");
    // `aws iam list-roles` returns Roles[*].AssumeRolePolicyDocument as a
    // URL-encoded JSON document. The corpus's `f` helper filters roles
    // whose trust policy has a Statement with Principal.Service matching
    // the provided domain.
    let trust_doc_eks = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"eks.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;
    let trust_doc_lambda = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"lambda.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;
    let stdout = format!(
        r#"{{"Roles":[{{"RoleName":"EksRole","AssumeRolePolicyDocument":"{}"}},{{"RoleName":"LambdaRole","AssumeRolePolicyDocument":"{}"}}]}}"#,
        percent_encode(trust_doc_eks),
        percent_encode(trust_doc_lambda),
    );
    let body = r#"t=>f(t,"eks.amazonaws.com")"#;
    let out = run(&worker, &post_process_program(body, &stdout)).await;
    let names: Vec<_> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["EksRole"],
        "f should keep only the role whose trust policy lists eks.amazonaws.com; diagnostics: {:?}",
        out.diagnostics
    );
}

// --- Robustness: bad input must not crash the sandbox ----------------------

#[tokio::test]
async fn helpers_return_empty_on_unparseable_stdout() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let stdout = "this is not JSON";
    let body = r#"function(t){return l(t,"Roles","RoleName")}"#;
    let out = run(&worker, &post_process_program(body, stdout)).await;
    assert!(
        out.suggestions().is_empty(),
        "expected empty suggestions on unparseable stdout, got {:?}",
        out.suggestions()
    );
}

#[tokio::test]
async fn helpers_return_empty_on_missing_array_field() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let stdout = r#"{"OtherField":[]}"#;
    let body = r#"function(t){return l(t,"Roles","RoleName")}"#;
    let out = run(&worker, &post_process_program(body, stdout)).await;
    assert!(
        out.suggestions().is_empty(),
        "diagnostics: {:?}",
        out.diagnostics
    );
}

// --- Sandbox-state hygiene: helpers cannot escape the per-job context -----

#[tokio::test]
async fn helpers_are_isolated_across_jobs() {
    // Verify that the helper preamble installed in one job does not leak
    // into a later job's freshly-created context. This is implicit from
    // the per-job Context::full(runtime) call in worker.rs, but we pin it
    // down: a body that monkey-patches `l` in job 1 must not affect job 2.
    let worker = JsWorker::spawn().expect("spawn worker");

    let monkey_patch =
        r#"(function(){ l = function(){ return [{name:'pwned'}]; }; return []; })()"#;
    let _ = run(&worker, monkey_patch).await;

    let stdout = r#"{"Roles":[{"RoleName":"clean"}]}"#;
    let body = r#"function(t){return l(t,"Roles","RoleName")}"#;
    let out = run(&worker, &post_process_program(body, stdout)).await;
    let names: Vec<_> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["clean"], "diagnostics: {:?}", out.diagnostics);
}
