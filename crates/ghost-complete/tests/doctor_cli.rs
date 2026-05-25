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
    // Assert the specific new check fires: it names the missing init.zsh path
    // or the shell-integration managed block. The pre-Task-10 implementation
    // would only emit a generic "managed block present" Ok and never reference
    // init.zsh by name from the shell-integration check.
    let lower = combined.to_lowercase();
    assert!(
        lower.contains("init.zsh")
            || lower.contains("shell-integration managed block")
            || lower.contains("ghost-complete.zsh"),
        "doctor must reference the missing managed-file path or block by name; got:\n{combined}",
    );
}

#[test]
fn doctor_passes_when_shell_integration_files_present_at_correct_path() {
    // Regression: the shell-integration check briefly looked for
    // init.zsh / ghost-complete.zsh directly under
    // ~/.config/ghost-complete/, but install writes them to the
    // shell/ subdirectory. Any user who actually ran
    // `ghost-complete install` would have seen doctor report
    // Fail: missing or unreadable for both files. This test
    // exercises the happy path — correct layout, embedded
    // snippets on disk — and asserts doctor exits 0 with no
    // [FAIL] lines.
    //
    // The managed-block source line format mirrors what
    // install.rs::init_block / shell_integration_block actually
    // writes: single-quoted, absolute paths. The parser ignores
    // non-`source` lines so the `if [[ -f '<path>' ]]; then` guard
    // around `builtin source '<path>'` is also covered here.
    use std::process::Command;
    use tempfile::TempDir;

    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root");
    let zsh_init = std::fs::read_to_string(repo_root.join("shell/init.zsh"))
        .expect("read shell/init.zsh from repo");
    let zsh_integration = std::fs::read_to_string(repo_root.join("shell/ghost-complete.zsh"))
        .expect("read shell/ghost-complete.zsh from repo");

    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    // Lay the install mirror down at the exact path install.rs writes:
    // ~/.config/ghost-complete/shell/{init.zsh,ghost-complete.zsh}.
    let shell_dir = home.join(".config/ghost-complete/shell");
    std::fs::create_dir_all(&shell_dir).unwrap();
    let init_path = shell_dir.join("init.zsh");
    let script_path = shell_dir.join("ghost-complete.zsh");
    std::fs::write(&init_path, &zsh_init).unwrap();
    std::fs::write(&script_path, &zsh_integration).unwrap();

    // .zshrc must contain BOTH managed blocks for check 1 to pass.
    // Marker strings mirror install.rs INIT_BEGIN/SHELL_BEGIN constants.
    // Source paths are single-quoted absolute paths exactly as
    // install.rs::shell_safe_path renders them.
    let zshrc = home.join(".zshrc");
    let zshrc_contents = format!(
        "# >>> ghost-complete initialize >>>\n\
         if [[ -f '{init}' ]]; then\n  \
         builtin source '{init}'\n\
         fi\n\
         # <<< ghost-complete initialize <<<\n\
         # >>> ghost-complete shell integration >>>\n\
         source '{script}'\n\
         # <<< ghost-complete shell integration <<<\n",
        init = init_path.display(),
        script = script_path.display(),
    );
    std::fs::write(&zshrc, &zshrc_contents).unwrap();

    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_ghost-complete"))
        .arg("--config")
        .arg(&cfg)
        .arg("doctor")
        .env("HOME", home)
        .output()
        .expect("spawn ghost-complete");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    assert!(
        output.status.success(),
        "doctor must exit 0 when shell integration is installed at the correct path \
         (~/.config/ghost-complete/shell/...).\nexit: {:?}\ncombined:\n{combined}",
        output.status.code(),
    );
    assert!(
        !combined.contains("[FAIL]"),
        "doctor must not report any [FAIL] lines on a clean install at the correct \
         path; got:\n{combined}",
    );
}

#[test]
fn doctor_fails_when_zshrc_sources_missing_file() {
    // Regression: doctor used to check the canonical install path
    // (~/.config/ghost-complete/shell/init.zsh) regardless of where
    // .zshrc actually sourced. A stale .zshrc pointing at a long-gone
    // path would silently pass if the canonical path happened to have
    // files. Now doctor extracts the source path from the managed block.
    use std::process::Command;
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let zshrc = home.join(".zshrc");
    std::fs::write(
        &zshrc,
        "# >>> ghost-complete initialize >>>\n\
         if [[ -f '/nonexistent/stale/init.zsh' ]]; then\n  \
         builtin source '/nonexistent/stale/init.zsh'\n\
         fi\n\
         # <<< ghost-complete initialize <<<\n\
         # >>> ghost-complete shell integration >>>\n\
         source '/nonexistent/stale/ghost-complete.zsh'\n\
         # <<< ghost-complete shell integration <<<\n",
    )
    .unwrap();

    // Also populate the canonical path so the previous (broken) check would
    // have passed. The new check should still fail because .zshrc sources
    // the stale paths, not these.
    let canonical = home.join(".config/ghost-complete/shell");
    std::fs::create_dir_all(&canonical).unwrap();
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap();
    std::fs::copy(repo_root.join("shell/init.zsh"), canonical.join("init.zsh")).unwrap();
    std::fs::copy(
        repo_root.join("shell/ghost-complete.zsh"),
        canonical.join("ghost-complete.zsh"),
    )
    .unwrap();

    let cfg = home.join("config.toml");
    std::fs::write(&cfg, "").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_ghost-complete"))
        .arg("--config")
        .arg(&cfg)
        .arg("doctor")
        .env("HOME", home)
        .output()
        .expect("spawn ghost-complete");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    assert!(
        !output.status.success(),
        "doctor must NOT succeed when .zshrc sources a missing file even \
         when canonical install path has files. Got exit success.\n{combined}",
    );
    assert!(
        combined.contains("/nonexistent/stale/") || combined.to_lowercase().contains("missing"),
        "doctor must surface the stale source path or 'missing'; got:\n{combined}",
    );
}

#[test]
fn doctor_warns_when_zshrc_sources_noncanonical_path() {
    // .zshrc sources working files at a non-canonical location.
    // doctor should warn (not fail) so the user notices the drift
    // before a future install gets confused.
    use std::process::Command;
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let alt_dir = home.join("alt/ghost-complete-files");
    std::fs::create_dir_all(&alt_dir).unwrap();

    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap();
    let zsh_init_contents = std::fs::read_to_string(repo_root.join("shell/init.zsh")).unwrap();
    let zsh_integration_contents =
        std::fs::read_to_string(repo_root.join("shell/ghost-complete.zsh")).unwrap();

    let alt_init = alt_dir.join("init.zsh");
    let alt_script = alt_dir.join("ghost-complete.zsh");
    std::fs::write(&alt_init, &zsh_init_contents).unwrap();
    std::fs::write(&alt_script, &zsh_integration_contents).unwrap();

    let zshrc = home.join(".zshrc");
    let zshrc_contents = format!(
        "# >>> ghost-complete initialize >>>\n\
         builtin source '{}'\n\
         # <<< ghost-complete initialize <<<\n\
         # >>> ghost-complete shell integration >>>\n\
         source '{}'\n\
         # <<< ghost-complete shell integration <<<\n",
        alt_init.display(),
        alt_script.display(),
    );
    std::fs::write(&zshrc, &zshrc_contents).unwrap();

    let cfg = home.join("config.toml");
    std::fs::write(&cfg, "").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_ghost-complete"))
        .arg("--config")
        .arg(&cfg)
        .arg("doctor")
        .env("HOME", home)
        .output()
        .expect("spawn ghost-complete");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    // Warn shouldn't push exit nonzero in the existing doctor (warn != fail).
    // Assert the noncanonical-path drift line appears.
    let lower = combined.to_lowercase();
    assert!(
        lower.contains("non-canonical")
            || lower.contains("canonical")
            || lower.contains("alt/ghost-complete-files"),
        "doctor must surface a drift warning for non-canonical source paths; \
         got:\n{combined}",
    );
}
