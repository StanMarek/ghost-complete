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
fn doctor_exits_zero_with_clean_config() {
    // Empty (default) config: keybindings/theme parse, embedded specs load,
    // JS runtime defaults to on. No Fail results expected — Warns from
    // missing shell integration / terminal detection are tolerated.
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "").unwrap();

    let output = Command::new(ghost_bin())
        .arg("--config")
        .arg(&cfg)
        .arg("doctor")
        .output()
        .expect("failed to spawn ghost-complete");

    assert_eq!(
        output.status.code(),
        Some(0),
        "doctor with empty config must exit 0.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
