//! Tripwire for the bundled QuickJS (quickjs-ng) engine's Unicode behavior.
//!
//! `requires_js` generators are arbitrary corpus JS; the JS engine is the
//! code path that turns spec JS into completion suggestions. A quickjs-ng
//! bump can advance the bundled Unicode tables — the rquickjs 0.10 -> 0.12
//! bump pulled in Unicode 17.0.0 — which shifts `String` case-folding /
//! normalization and `RegExp` Unicode-property matching for non-ASCII
//! input. A generator that lowercases, normalizes, or regex-filters branch
//! names, tags, or paths would then silently emit different suggestions.
//!
//! There are no golden-output tests over the real corpus generators, so
//! this test pins the current engine's behavior on a representative set of
//! Unicode-sensitive operations. A future engine bump that changes any of
//! them fails HERE, forcing a human to re-audit generator output rather
//! than discovering the drift in production.

use std::time::Duration;

use gc_jsrt::{JsRuntimeInput, JsWorker};

const FAST_TIMEOUT: Duration = Duration::from_millis(1_500);

fn tripwire_input() -> JsRuntimeInput {
    JsRuntimeInput {
        generator_id: "unicode-tripwire".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn engine_unicode_behavior_is_pinned() {
    let worker = JsWorker::spawn().expect("spawn worker");
    // Each check yields "<key>=<result>" so a drift report names exactly
    // which Unicode operation changed.
    let program = r#"(function(){
        const checks = [
            ["sharp-s-upper",     "ß".toUpperCase()],
            ["cap-e-acute-lower", "É".toLowerCase()],
            ["dotted-I-lower",    "İ".toLowerCase()],
            ["nfc",               "é".normalize("NFC")],
            ["nfd",               "é".normalize("NFD")],
            ["prop-letter",       String(/^\p{L}+$/u.test("café日本ß"))],
            ["prop-decimal",      String(/\p{Nd}/u.test("٥"))],
            ["han-script",        String(/\p{Script=Han}/u.test("日"))]
        ];
        return checks.map(function(c){ return {name: c[0] + "=" + c[1]}; });
    })()"#;
    let out = worker
        .evaluate(program, tripwire_input(), FAST_TIMEOUT)
        .await
        .expect("evaluation infrastructure should not fail");
    let names: Vec<_> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "sharp-s-upper=SS",
            "cap-e-acute-lower=\u{e9}",
            "dotted-I-lower=i\u{307}",
            "nfc=\u{e9}",
            "nfd=e\u{301}",
            "prop-letter=true",
            "prop-decimal=true",
            "han-script=true",
        ],
        "QuickJS (quickjs-ng) Unicode behavior drifted after an engine bump; \
         re-audit requires_js generator output for non-ASCII input. diagnostics: {:?}",
        out.diagnostics
    );
}
