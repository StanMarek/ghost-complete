//! Integration tests for the Fig-compatible single-letter helper preamble
//! installed in the QuickJS sandbox before every job.
//!
//! Background: Fig's `@withfig/autocomplete` bundler emits sub-spec
//! modules with helper functions (typed as `ListOutputGenerator`,
//! `PathOutputGenerator`, etc.) minified down to single letters
//! (`l`, `p`, `c`, `d`, `h`, `f`). The post_process bodies the converter
//! preserved reference those letters by name. Without these definitions,
//! the QuickJS sandbox throws `ReferenceError` on every preserved
//! post_process body that references a helper and silently emits zero
//! suggestions. The preamble (see crates/gc-jsrt/src/helpers.js) installs
//! pure-JS definitions before each evaluation.
//!
//! These tests pin down the helper semantics derived from the AWS
//! corpus: each is a pure JSON walker over a JSON-formatted stdout
//! payload. None of them call `executeShellCommand`, `fetch`, or any
//! other host binding — that property is enforced by construction in
//! helpers.js, which references neither `executeShellCommand`, `fetch`,
//! nor `fig.*`; the dedicated isolation test `helpers_are_isolated_across_jobs`
//! pins the per-job freshness invariant.

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

/// 4-arity helper bodies project a description field. The converter's
/// helper-matcher emits a `json_path_extract` with `description_field`
/// set, so the JS fallback path must produce the same {name,
/// description} shape — otherwise specs that route through JS would
/// silently lose descriptions relative to their lowered counterparts.
#[tokio::test]
async fn helper_l_four_arg_form_extracts_name_and_description() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let stdout = r#"{"Roles":[{"RoleName":"admin","Arn":"arn:aws:iam::1:role/admin"},{"RoleName":"viewer","Arn":"arn:aws:iam::1:role/viewer"}]}"#;
    let body = r#"function(t){return l(t,"Roles","RoleName","Arn")}"#;
    let out = run(&worker, &post_process_program(body, stdout)).await;
    let pairs: Vec<_> = out
        .suggestions()
        .iter()
        .map(|s| (s.name.as_str(), s.description.as_deref().unwrap_or("")))
        .collect();
    assert_eq!(
        pairs,
        [
            ("admin", "arn:aws:iam::1:role/admin"),
            ("viewer", "arn:aws:iam::1:role/viewer"),
        ],
        "diagnostics: {:?}",
        out.diagnostics
    );
}

/// A 4-arity call where the description field is absent on a row
/// should still emit a name-only suggestion for that row — losing the
/// description is preferable to dropping the row entirely.
#[tokio::test]
async fn helper_l_four_arg_form_omits_description_when_missing() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let stdout = r#"{"Roles":[{"RoleName":"admin","Arn":"arn:role/admin"},{"RoleName":"viewer"}]}"#;
    let body = r#"function(t){return l(t,"Roles","RoleName","Arn")}"#;
    let out = run(&worker, &post_process_program(body, stdout)).await;
    let pairs: Vec<_> = out
        .suggestions()
        .iter()
        .map(|s| (s.name.as_str(), s.description.clone()))
        .collect();
    assert_eq!(
        pairs,
        [("admin", Some("arn:role/admin".into())), ("viewer", None),]
    );
}

// --- f: filter IAM roles by AssumeRolePolicyDocument Principal.Service ----

/// Percent-encode non-unreserved bytes (RFC 3986 unreserved set).
/// `aws iam list-roles` returns AssumeRolePolicyDocument URL-encoded;
/// this matches what the AWS API emits closely enough for fixture
/// purposes.
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

#[tokio::test]
async fn helper_f_filters_when_principal_service_is_array() {
    // AWS IAM trust policies routinely use the array form for
    // `Principal.Service` when a role can be assumed by multiple
    // services. The `f` helper's `Array.isArray(svc)` branch handles
    // this; this test pins it.
    let worker = JsWorker::spawn().expect("spawn worker");
    let trust_doc = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":["eks.amazonaws.com","ecs-tasks.amazonaws.com"]},"Action":"sts:AssumeRole"}]}"#;
    let stdout = format!(
        r#"{{"Roles":[{{"RoleName":"MultiServiceRole","AssumeRolePolicyDocument":"{}"}}]}}"#,
        percent_encode(trust_doc),
    );

    let body_match = r#"t=>f(t,"ecs-tasks.amazonaws.com")"#;
    let out = run(&worker, &post_process_program(body_match, &stdout)).await;
    let names: Vec<_> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["MultiServiceRole"],
        "f should match when the requested principal is in the Service array; diagnostics: {:?}",
        out.diagnostics
    );

    let body_miss = r#"t=>f(t,"sns.amazonaws.com")"#;
    let out = run(&worker, &post_process_program(body_miss, &stdout)).await;
    assert!(
        out.suggestions().is_empty(),
        "f should not match when the requested principal is absent from the Service array; got {:?}, diagnostics: {:?}",
        out.suggestions(),
        out.diagnostics,
    );
}

#[tokio::test]
async fn helper_f_accepts_preparsed_document_object() {
    // Some AWS SDK responses surface `AssumeRolePolicyDocument` as an
    // already-deserialized object rather than a URL-encoded string. The
    // `f` helper's `if (typeof doc === "string")` guard intentionally
    // falls through in that case; this test pins the object-form path.
    let worker = JsWorker::spawn().expect("spawn worker");
    let stdout = r#"{"Roles":[{"RoleName":"R","AssumeRolePolicyDocument":{"Version":"2012-10-17","Statement":[{"Principal":{"Service":"eks.amazonaws.com"},"Effect":"Allow"}]}}]}"#;
    let body = r#"t=>f(t,"eks.amazonaws.com")"#;
    let out = run(&worker, &post_process_program(body, stdout)).await;
    let names: Vec<_> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["R"],
        "f should accept a pre-parsed object form of AssumeRolePolicyDocument; diagnostics: {:?}",
        out.diagnostics
    );
}

#[tokio::test]
async fn helper_f_skips_role_with_malformed_document_and_keeps_going() {
    // The `f` helper's defining defensive property: when one role's
    // AssumeRolePolicyDocument fails to decode/parse (truncated percent
    // escape, garbage payload, etc.), the helper `continue`s to the next
    // role rather than aborting the whole filter. A regression that
    // replaced `continue` with `return []` or a throw would silently
    // strip ALL roles whenever a single malformed document appears in
    // the response — exactly the kind of partial-data scenario that
    // surfaces during eventual-consistency windows.
    //
    // Fixture: two roles. The first has a non-percent-encoded garbage
    // string in AssumeRolePolicyDocument (decodes fine, then JSON.parse
    // throws). The second has a well-formed eks trust policy. The
    // helper must skip the first and surface the second.
    let worker = JsWorker::spawn().expect("spawn worker");
    let trust_doc_eks = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"eks.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;
    let stdout = format!(
        r#"{{"Roles":[{{"RoleName":"BrokenRole","AssumeRolePolicyDocument":"not%20json%20at%20all"}},{{"RoleName":"EksRole","AssumeRolePolicyDocument":"{}"}}]}}"#,
        percent_encode(trust_doc_eks),
    );
    let body = r#"t=>f(t,"eks.amazonaws.com")"#;
    let out = run(&worker, &post_process_program(body, &stdout)).await;
    let names: Vec<_> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["EksRole"],
        "f must skip the role with the malformed AssumeRolePolicyDocument and continue to the next role, not abort the whole filter; diagnostics: {:?}",
        out.diagnostics
    );
}

// --- listExtract: defensive null / coercion paths -------------------------

#[tokio::test]
async fn helpers_skip_items_missing_named_field() {
    // Real AWS responses include items missing the requested field
    // during eventual-consistency windows. `listExtract` defensively
    // skips null items, non-object items, and items whose named field
    // is null/undefined. This test pins those branches: nothing should
    // surface as `{name: "null"}` or `{name: "undefined"}`.
    let worker = JsWorker::spawn().expect("spawn worker");
    let stdout =
        r#"{"Roles":[{"RoleName":"a"},{"RoleName":null},null,{"OtherKey":"x"},{"RoleName":"b"}]}"#;
    let body = r#"function(t){return l(t,"Roles","RoleName")}"#;
    let out = run(&worker, &post_process_program(body, stdout)).await;
    let names: Vec<_> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["a", "b"],
        "listExtract should silently drop null-name, null-item, and missing-field rows; diagnostics: {:?}",
        out.diagnostics
    );
}

#[tokio::test]
async fn helper_two_arg_form_coerces_and_skips_null() {
    // Per helpers.js, the 2-arg form coerces every kept element via
    // `String(v)` and skips `null`. All five aliases share the same
    // `listExtract` body, so exercising the `l` binding covers the
    // 2-arg branch for the whole family.
    let worker = JsWorker::spawn().expect("spawn worker");
    let stdout = r#"{"Versions":[1,null,"three"]}"#;
    let body = r#"function(t){return l(t,"Versions")}"#;
    let out = run(&worker, &post_process_program(body, stdout)).await;
    let names: Vec<_> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["1", "three"],
        "2-arg listExtract should coerce numbers via String() and skip null; diagnostics: {:?}",
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
    //
    // The monkey_patch program also self-checks that its replacement of
    // `l` took effect within job 1 — without this check, a future
    // strict-mode flip that turned undeclared-global assignment into a
    // TypeError would let the test pass while quietly no longer
    // exercising the isolation invariant.
    let worker = JsWorker::spawn().expect("spawn worker");

    let monkey_patch = r#"(function(){
        l = function(){ return [{name:'pwned'}]; };
        return [{name: l('','','').length === 1 && l('','','')[0].name === 'pwned'
            ? 'patch-installed'
            : 'patch-failed'}];
    })()"#;
    let job1_out = run(&worker, monkey_patch).await;
    let job1_names: Vec<_> = job1_out
        .suggestions()
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(
        job1_names,
        ["patch-installed"],
        "job 1 must successfully install the monkey-patch for the isolation test to be meaningful; diagnostics: {:?}",
        job1_out.diagnostics
    );

    let stdout = r#"{"Roles":[{"RoleName":"clean"}]}"#;
    let body = r#"function(t){return l(t,"Roles","RoleName")}"#;
    let out = run(&worker, &post_process_program(body, stdout)).await;
    let names: Vec<_> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["clean"], "diagnostics: {:?}", out.diagnostics);
}
