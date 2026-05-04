//! Smoke tests for the `ghost-complete doctor` CLI exit semantics.
//!
//! Unit tests cover individual `check_*` helpers; this file exercises
//! `run_doctor` end-to-end via the binary so the orchestration logic
//! (skip-when-config-invalid, exit code 1 on any Fail) stays honest.

use std::process::Command;

fn ghost_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ghost-complete")
}

#[test]
fn doctor_exits_nonzero_when_config_is_malformed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "this = is = not = valid = toml").unwrap();

    let output = Command::new(ghost_bin())
        .arg("--config")
        .arg(&cfg)
        .arg("doctor")
        .output()
        .expect("failed to spawn ghost-complete");

    assert_eq!(
        output.status.code(),
        Some(1),
        "doctor must exit 1 when config fails to load.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[FAIL]"),
        "expected at least one [FAIL] line for malformed config, got:\n{stdout}"
    );
    assert!(
        stdout.contains("[SKIP]"),
        "checks dependent on a valid config must skip, got:\n{stdout}"
    );
}

#[test]
fn doctor_with_clean_config_runs_to_completion() {
    // Empty (default) config: keybindings/theme parse, embedded specs load,
    // JS runtime defaults to on. The exit code depends on the embedded
    // corpus's runtime-metadata health — the converted v0.12.x corpus
    // currently ships ~1697 `script_function` / `custom` generators that
    // lack the `self_contained:true` proof, so the runtime-metadata
    // check Fails and doctor exits 1. That's the truthful state the
    // engine actually dispatches against (see `check_embedded_runtime_metadata`
    // and `js_runtime_supported` in gc-suggest::engine). The CRITICAL
    // assertion this test guards against is that doctor doesn't panic
    // or hang — both 0 (corpus clean) and 1 (corpus defect surfaced) are
    // valid outcomes; only an absent / negative status code is a regression.
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "").unwrap();

    let output = Command::new(ghost_bin())
        .arg("--config")
        .arg(&cfg)
        .arg("doctor")
        .output()
        .expect("failed to spawn ghost-complete");

    let code = output.status.code();
    assert!(
        matches!(code, Some(0) | Some(1)),
        "doctor with empty config must exit 0 (clean corpus) or 1 (corpus \
         defect surfaced) — never crash or signal-exit.\nexit: {code:?}\n\
         stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Doctor must always emit its banner header — confirms the binary
    // ran the doctor flow rather than dying before reaching it.
    assert!(
        stdout.contains("Ghost Complete Doctor"),
        "doctor must emit its banner, got stdout:\n{stdout}"
    );
}
