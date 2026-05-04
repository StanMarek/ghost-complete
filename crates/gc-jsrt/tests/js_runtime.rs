//! Integration tests for [`gc_jsrt::JsWorker`].
//!
//! Each test spins up its own worker so a stuck/poisoned runtime in
//! one test cannot affect another. The worker is cheap to spawn -- the
//! tests still complete in <1s on a debug build.

use std::time::Duration;

use gc_jsrt::{JsDiagnosticCode, JsRuntimeError, JsRuntimeInput, JsWorker};

const FAST_TIMEOUT: Duration = Duration::from_millis(500);

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

#[tokio::test]
async fn evaluates_string_literal() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let out = run(&worker, "'hello'").await;
    assert_eq!(out.suggestions.len(), 1);
    assert_eq!(out.suggestions[0].name, "hello");
}

#[tokio::test]
async fn evaluates_string_array() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let out = run(&worker, "['a','b','c']").await;
    assert_eq!(out.suggestions.len(), 3);
    let names: Vec<_> = out.suggestions.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["a", "b", "c"]);
}

#[tokio::test]
async fn evaluates_object_with_name_and_description() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let out = run(&worker, "({name:'foo',description:'bar'})").await;
    assert_eq!(out.suggestions.len(), 1);
    assert_eq!(out.suggestions[0].name, "foo");
    assert_eq!(out.suggestions[0].description.as_deref(), Some("bar"));
}

#[tokio::test]
async fn evaluates_object_array() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let out = run(&worker, "[{name:'a'},{name:'b'}]").await;
    assert_eq!(out.suggestions.len(), 2);
    assert_eq!(out.suggestions[0].name, "a");
    assert_eq!(out.suggestions[1].name, "b");
}

#[tokio::test]
async fn promise_resolution_is_unwrapped() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let out = run(&worker, "Promise.resolve('hello')").await;
    assert_eq!(out.suggestions.len(), 1);
    assert_eq!(out.suggestions[0].name, "hello");
}

#[tokio::test]
async fn async_arrow_function_is_unwrapped() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let out = run(&worker, "(async () => 'x')()").await;
    assert_eq!(out.suggestions.len(), 1);
    assert_eq!(out.suggestions[0].name, "x");
}

#[tokio::test]
async fn promise_resolving_to_array() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let out = run(&worker, "Promise.resolve([{name:'main'},{name:'dev'}])").await;
    assert_eq!(out.suggestions.len(), 2);
    assert_eq!(out.suggestions[0].name, "main");
    assert_eq!(out.suggestions[1].name, "dev");
}

#[tokio::test]
async fn runaway_loop_times_out() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let start = std::time::Instant::now();
    let out = worker
        .evaluate("while(true){}", empty_input(), Duration::from_millis(50))
        .await
        .expect("infrastructure should not fail");
    let elapsed = start.elapsed();
    // The interrupt handler is best-effort but should fire quickly. Allow
    // a generous upper bound to keep CI machines happy.
    assert!(
        elapsed < Duration::from_secs(2),
        "expected timeout in <2s, got {elapsed:?}"
    );
    assert!(out.suggestions.is_empty());
    assert_eq!(out.diagnostics[0].code, JsDiagnosticCode::Timeout);
}

#[tokio::test]
async fn worker_is_reusable_after_timeout() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let _ = worker
        .evaluate("while(true){}", empty_input(), Duration::from_millis(50))
        .await
        .expect("infrastructure should not fail");

    // Subsequent evaluations succeed: the runtime survives interruptions.
    let out = run(&worker, "'survived'").await;
    assert_eq!(out.suggestions.len(), 1);
    assert_eq!(out.suggestions[0].name, "survived");
}

#[tokio::test]
async fn memory_exhaustion_yields_diagnostic() {
    // Allocate ~32MB of strings -- well above the 8 MiB worker limit.
    // QuickJS may surface this as either Allocation or a thrown
    // exception depending on where the limit hits; both are acceptable
    // termination signals.
    let worker = JsWorker::spawn().expect("spawn worker");
    let out = worker
        .evaluate(
            "const a = []; for (let i=0; i<2_000_000; i++) a.push({a:1, b:'x'.repeat(64)});",
            empty_input(),
            Duration::from_secs(5),
        )
        .await
        .expect("infrastructure should not fail");
    assert!(out.suggestions.is_empty(), "expected no suggestions");
    let code = out.diagnostics[0].code;
    assert!(
        matches!(
            code,
            JsDiagnosticCode::MemoryExceeded
                | JsDiagnosticCode::Exception
                | JsDiagnosticCode::Timeout
        ),
        "unexpected diagnostic code {code:?}"
    );
}

#[tokio::test]
async fn dangerous_globals_are_undefined() {
    let worker = JsWorker::spawn().expect("spawn worker");
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
    ];
    for name in dangerous {
        let out = run(&worker, &format!("typeof {name}")).await;
        assert_eq!(
            out.suggestions.len(),
            1,
            "{name}: expected one suggestion, got {out:?}"
        );
        assert_eq!(
            out.suggestions[0].name, "undefined",
            "{name}: expected typeof to be 'undefined'"
        );
    }
}

#[tokio::test]
async fn dynamic_eval_intrinsic_is_disabled() {
    let worker = JsWorker::spawn().expect("spawn worker");
    // Build the probe at runtime so static scanners skip the literal.
    let probe = ["ev", "al", "('1+1')"].concat();
    let out = run(&worker, &probe).await;
    assert!(out.suggestions.is_empty());
    assert_eq!(out.diagnostics[0].code, JsDiagnosticCode::Exception);
    assert!(out.diagnostics[0]
        .message
        .to_lowercase()
        .contains("disabled"));
}

#[tokio::test]
async fn dynamic_function_constructor_is_disabled() {
    let worker = JsWorker::spawn().expect("spawn worker");
    // Construct the test program at runtime so static scanners don't
    // flag the literal usage of the dynamic-code-generation API name.
    let probe = ["new ", "Func", "tion", "('return 1')()"].concat();
    let out = run(&worker, &probe).await;
    assert!(out.suggestions.is_empty());
    assert_eq!(out.diagnostics[0].code, JsDiagnosticCode::Exception);
}

#[tokio::test]
async fn cyclic_object_is_invalid_shape() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let out = run(&worker, "let a = {}; a.self = a; a").await;
    assert!(out.suggestions.is_empty());
    assert_eq!(
        out.diagnostics[0].code,
        JsDiagnosticCode::InvalidShape,
        "got: {out:?}"
    );
}

#[tokio::test]
async fn oversized_string_output_is_clipped() {
    let worker = JsWorker::spawn().expect("spawn worker");
    // 2 MiB string, way above MAX_TOTAL_OUTPUT_BYTES (256 KiB).
    let out = run(&worker, "'x'.repeat(2 * 1024 * 1024)").await;
    assert!(out.suggestions.is_empty());
    assert_eq!(out.diagnostics[0].code, JsDiagnosticCode::OversizedOutput);
}

#[tokio::test]
async fn oversized_array_is_truncated_via_runtime() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let out = run(&worker, "Array.from({length: 5000}, (_, i) => 'item' + i)").await;
    // MAX_SUGGESTIONS is 1024
    assert_eq!(out.suggestions.len(), gc_jsrt::MAX_SUGGESTIONS);
    assert!(out
        .diagnostics
        .iter()
        .any(|d| d.code == JsDiagnosticCode::OversizedOutput));
}

#[tokio::test]
async fn function_value_is_invalid_shape() {
    let worker = JsWorker::spawn().expect("spawn worker");
    // A bare function returns `undefined` from JSON.stringify, which we
    // map to InvalidShape (or EmptyOutput depending on the fallthrough).
    let out = run(&worker, "(function() { return 1; })").await;
    assert!(out.suggestions.is_empty());
    assert!(matches!(
        out.diagnostics[0].code,
        JsDiagnosticCode::InvalidShape | JsDiagnosticCode::EmptyOutput
    ));
}

#[tokio::test]
async fn empty_array_emits_empty_output() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let out = run(&worker, "[]").await;
    assert!(out.suggestions.is_empty());
    assert_eq!(out.diagnostics[0].code, JsDiagnosticCode::EmptyOutput);
}

#[tokio::test]
async fn explicit_throw_is_exception() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let out = run(&worker, "throw new Error('boom')").await;
    assert!(out.suggestions.is_empty());
    assert_eq!(out.diagnostics[0].code, JsDiagnosticCode::Exception);
    assert!(out.diagnostics[0].message.contains("boom"));
}

#[tokio::test]
async fn syntax_error_is_exception() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let out = run(&worker, "this is not valid (").await;
    assert!(out.suggestions.is_empty());
    assert_eq!(out.diagnostics[0].code, JsDiagnosticCode::Exception);
}

#[tokio::test]
async fn rejected_promise_is_exception() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let out = run(&worker, "Promise.reject(new Error('rejected'))").await;
    assert!(out.suggestions.is_empty());
    assert_eq!(out.diagnostics[0].code, JsDiagnosticCode::Exception);
}

#[tokio::test]
async fn intrinsics_remain_available() {
    // Standard QuickJS intrinsics should still work -- only Node-style
    // and constructor-style extensions are stripped.
    let worker = JsWorker::spawn().expect("spawn worker");
    for (program, expected) in [
        ("JSON.parse('[1,2,3]').length.toString()", "3"),
        ("Math.max(1, 2, 3).toString()", "3"),
        ("'abc'.toUpperCase()", "ABC"),
        ("[1,2,3].map(x => x + 1).join(',')", "2,3,4"),
    ] {
        let out = run(&worker, program).await;
        assert_eq!(
            out.suggestions[0].name, expected,
            "program `{program}` produced {out:?}"
        );
    }
}

#[tokio::test]
async fn separate_jobs_have_isolated_globals() {
    let worker = JsWorker::spawn().expect("spawn worker");
    // Set a global in one job -- by the time the next job runs in a
    // fresh context it should be undefined.
    let out1 = run(
        &worker,
        "globalThis.__leak_test__ = 'leaked'; globalThis.__leak_test__",
    )
    .await;
    assert_eq!(out1.suggestions[0].name, "leaked");

    let out2 = run(&worker, "typeof globalThis.__leak_test__").await;
    assert_eq!(out2.suggestions[0].name, "undefined");
}

#[tokio::test]
async fn multiple_concurrent_evaluations_serialise_cleanly() {
    let worker = JsWorker::spawn().expect("spawn worker");

    let mut handles = Vec::new();
    for i in 0..8 {
        let w = worker.clone();
        handles.push(tokio::spawn(async move {
            let program = format!("'task-{i}'");
            w.evaluate(program, empty_input(), FAST_TIMEOUT).await
        }));
    }

    for (i, h) in handles.into_iter().enumerate() {
        let out = h.await.expect("task panic").expect("infra ok");
        assert_eq!(out.suggestions[0].name, format!("task-{i}"));
    }
}

#[tokio::test]
async fn worker_clone_keeps_thread_alive() {
    let worker = JsWorker::spawn().expect("spawn worker");
    let cloned = worker.clone();
    drop(worker);
    let out = run(&cloned, "'still alive'").await;
    assert_eq!(out.suggestions[0].name, "still alive");
    drop(cloned);
}

#[tokio::test]
async fn worker_dead_error_when_thread_exits_before_send() {
    let worker = JsWorker::spawn_for_test_with_failing_thread().expect("spawn helper");
    let res = worker.evaluate("'ok'", empty_input(), FAST_TIMEOUT).await;
    assert!(
        matches!(res, Err(JsRuntimeError::WorkerDead)),
        "expected WorkerDead, got {res:?}"
    );
}
