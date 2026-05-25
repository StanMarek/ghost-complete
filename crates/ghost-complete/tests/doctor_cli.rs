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
    // JS runtime defaults to on. The converted v0.12.x corpus currently
    // ships some `script_function` / `custom` generators that lack the
    // `self_contained:true` proof; those are expected unsupported coverage
    // and should remain OK rather than making a clean local install noisy.
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "").unwrap();

    // Pin HOME to a clean temp dir so the shell-integration check (which
    // probes `~/.zshrc` + `~/.config/ghost-complete/{init,ghost-complete}.zsh`)
    // returns a deterministic Skip — without this pin, a dev whose local
    // .zshrc has half-installed managed blocks would see this test flap
    // when check_shell_integration starts surfacing Fail results.
    let output = Command::new(ghost_bin())
        .arg("--config")
        .arg(&cfg)
        .arg("doctor")
        .env("HOME", tmp.path())
        .output()
        .expect("failed to spawn ghost-complete");

    assert_eq!(
        output.status.code(),
        Some(0),
        "doctor with empty config must exit 0; unsupported JS coverage should not warn or fail.\n\
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

#[test]
fn doctor_warns_on_stale_init_block() {
    use std::process::Command;
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let zshrc = home.join(".zshrc");
    // Write a managed block with no matching managed files.
    std::fs::write(
        &zshrc,
        "# >>> ghost-complete initialize >>>\nsource ~/.config/ghost-complete/missing-init.zsh\n# <<< ghost-complete initialize <<<\n",
    )
    .unwrap();

    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_ghost-complete"))
        .arg("--config")
        .arg(&cfg)
        .arg("doctor")
        .env("HOME", home)
        .output()
        .expect("spawn ghost-complete");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stderr}{stdout}");
    assert!(
        combined.to_lowercase().contains("missing")
            || combined.to_lowercase().contains("not found")
            || combined.to_lowercase().contains("stale"),
        "doctor must warn about missing source target; got:\n{combined}",
    );
}
