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
    // Discriminates from the older check that emitted only a generic
    // "managed block present" Ok without naming the missing managed-file
    // path. The missing SHELL_BEGIN block is a Fail (exit 1), not a Warn —
    // pin both the exit code and a [FAIL] marker so a regression that
    // demotes the severity is caught.
    assert_eq!(
        output.status.code(),
        Some(1),
        "doctor must exit 1 when the shell-integration managed block is \
         missing.\ncombined:\n{combined}",
    );
    assert!(
        combined.contains("[FAIL]"),
        "doctor must emit a [FAIL] line for the missing managed block; \
         got:\n{combined}",
    );
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
fn doctor_accepts_legacy_double_quoted_source_paths() {
    // Regression: pre-shell_safe_path versions (roughly v0.6.1–v0.7.0) wrote
    // managed blocks with raw `path.display()` inside double quotes. The
    // single-quote-only parser would mis-handle those installs as "no
    // parseable source line". This test pins the legacy double-quoted form as
    // accepted — the canonical files are present and the parser must surface
    // them through to the existence check.
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
    let shell_dir = home.join(".config/ghost-complete/shell");
    std::fs::create_dir_all(&shell_dir).unwrap();
    let init_path = shell_dir.join("init.zsh");
    let script_path = shell_dir.join("ghost-complete.zsh");
    std::fs::write(&init_path, &zsh_init).unwrap();
    std::fs::write(&script_path, &zsh_integration).unwrap();

    // Legacy v0.6.1–v0.7.0 .zshrc shape: double-quoted source paths. The
    // init block wrapped `builtin source` inside `if [[ -f ... ]]; then ...
    // fi`; the shell-integration block was a single bare `source` line.
    // Mirror both forms so the parser exercises double-quote handling on
    // both blocks.
    let zshrc = home.join(".zshrc");
    let zshrc_contents = format!(
        "# >>> ghost-complete initialize >>>\n\
         if [[ -f \"{init}\" ]]; then\n  \
         builtin source \"{init}\"\n\
         fi\n\
         # <<< ghost-complete initialize <<<\n\
         # >>> ghost-complete shell integration >>>\n\
         source \"{script}\"\n\
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
        "doctor must exit 0 when shell integration uses legacy double-quoted source \
         paths and the referenced files exist.\nexit: {:?}\ncombined:\n{combined}",
        output.status.code(),
    );
    assert!(
        !combined.contains("[FAIL]"),
        "doctor must not report any [FAIL] lines on a legacy double-quoted install with \
         files at the canonical path; got:\n{combined}",
    );
    assert!(
        !combined.contains("no parseable"),
        "doctor must not surface 'no parseable source line' for double-quoted paths; \
         got:\n{combined}",
    );
}

#[test]
fn doctor_fails_legacy_double_quoted_path_when_file_missing() {
    // Companion to doctor_accepts_legacy_double_quoted_source_paths: the
    // parser still extracts the path from double-quoted source lines, but
    // when the referenced file is absent the existence check Fails. The
    // discriminator is the file check, not the quoting style.
    //
    // Pre-fix behavior on this fixture: parser returned None for both
    // blocks → Fail with "no parseable source line" → exit 1 → but the
    // failure message would NOT contain the stale path. Post-fix: parser
    // extracts the stale path, existence check Fails → exit 1 → the
    // failure message SHOULD contain the stale path or "missing".
    use std::process::Command;
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let zshrc = home.join(".zshrc");
    std::fs::write(
        &zshrc,
        "# >>> ghost-complete initialize >>>\n\
         if [[ -f \"/nonexistent/legacy/init.zsh\" ]]; then\n  \
         builtin source \"/nonexistent/legacy/init.zsh\"\n\
         fi\n\
         # <<< ghost-complete initialize <<<\n\
         # >>> ghost-complete shell integration >>>\n\
         source \"/nonexistent/legacy/ghost-complete.zsh\"\n\
         # <<< ghost-complete shell integration <<<\n",
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
        "doctor must NOT succeed when legacy double-quoted source paths reference \
         missing files. Got exit success.\n{combined}",
    );
    // The post-fix path either names the stale path (the existence-check Fail
    // message includes it via `path.display()`) or says "missing/unreadable".
    // The pre-fix path emits "no parseable source line" with neither marker —
    // assert against the post-fix wording so this test discriminates.
    let lower = combined.to_lowercase();
    assert!(
        combined.contains("/nonexistent/legacy/") || lower.contains("missing"),
        "doctor must surface the stale double-quoted source path or 'missing'; \
         got:\n{combined}",
    );
    assert!(
        !lower.contains("no parseable"),
        "doctor must NOT report 'no parseable source line' for double-quoted paths; \
         got:\n{combined}",
    );
}

#[test]
fn doctor_warns_on_pre_v0_9_exec_install_style() {
    // The original (pre-v0.9) installer wrote a managed block with no
    // `source` line at all — just an inline `exec ghost-complete`. There
    // are no external script files to verify. The post-fix doctor should
    // surface this as a Warn with a migration nudge (not a Fail), so
    // older installs keep doctor exit 0 while still being told to
    // upgrade.
    //
    // Pre-fix behavior: parser returns None → Fail "no parseable source
    // line" → exit 1. Post-fix: both blocks return None → Warn → exit 0.
    use std::process::Command;
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let zshrc = home.join(".zshrc");
    let exec_body = "if [[ -z \"$GHOST_COMPLETE_ACTIVE\" ]]; then\n   \
                     export GHOST_COMPLETE_ACTIVE=1\n   \
                     exec ghost-complete\nfi\n";
    let zshrc_contents = format!(
        "# >>> ghost-complete initialize >>>\n{exec_body}# <<< ghost-complete initialize <<<\n\
         # >>> ghost-complete shell integration >>>\n{exec_body}# <<< ghost-complete shell integration <<<\n",
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
    assert!(
        output.status.success(),
        "doctor must exit 0 on the pre-v0.9 exec-style install (Warn, not Fail).\n\
         exit: {:?}\ncombined:\n{combined}",
        output.status.code(),
    );
    let lower = combined.to_lowercase();
    assert!(
        lower.contains("[warn]"),
        "doctor must emit a [WARN] line for the pre-v0.9 install style; \
         got:\n{combined}",
    );
    assert!(
        lower.contains("pre-v0.9") || lower.contains("migrate"),
        "doctor must nudge the user toward `ghost-complete install` to migrate; \
         got:\n{combined}",
    );
    assert!(
        !lower.contains("no parseable"),
        "doctor must NOT report 'no parseable source line' for the pre-v0.9 install; \
         got:\n{combined}",
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
    // Assert the severity is Warn (not Fail) and that the noncanonical-path
    // drift line appears.
    assert!(
        output.status.success(),
        "non-canonical drift must be Warn, not Fail (exit 0).\n\
         exit: {:?}\ncombined:\n{combined}",
        output.status.code(),
    );
    assert!(
        combined.contains("[WARN]"),
        "doctor must emit a [WARN] line for non-canonical source paths; \
         got:\n{combined}",
    );
    let lower = combined.to_lowercase();
    assert!(
        lower.contains("non-canonical")
            || lower.contains("canonical")
            || lower.contains("alt/ghost-complete-files"),
        "doctor must surface a drift warning for non-canonical source paths; \
         got:\n{combined}",
    );
}

#[test]
fn doctor_fails_on_duplicate_init_block() {
    // A botched manual edit can leave two `# >>> ghost-complete initialize >>>`
    // markers in .zshrc. The doctor must Fail (exit 1) and surface the
    // duplicate so the user knows to uninstall+reinstall rather than
    // silently picking the first block.
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let zshrc = home.join(".zshrc");
    let block = "# >>> ghost-complete initialize >>>\n\
                 source '/home/user/init.zsh'\n\
                 # <<< ghost-complete initialize <<<\n";
    let shell_block = "# >>> ghost-complete shell integration >>>\n\
                       source '/home/user/ghost-complete.zsh'\n\
                       # <<< ghost-complete shell integration <<<\n";
    std::fs::write(&zshrc, format!("{block}{shell_block}{block}")).unwrap();

    let cfg = home.join("config.toml");
    std::fs::write(&cfg, "").unwrap();

    let output = Command::new(ghost_bin())
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
    assert_eq!(
        output.status.code(),
        Some(1),
        "doctor must exit 1 on duplicate init blocks.\ncombined:\n{combined}",
    );
    assert!(
        combined.contains("[FAIL]"),
        "doctor must emit [FAIL] for duplicate init blocks; got:\n{combined}",
    );
    let lower = combined.to_lowercase();
    assert!(
        lower.contains("duplicate"),
        "doctor must surface 'duplicate' in the message; got:\n{combined}",
    );
}

#[test]
fn doctor_fails_on_duplicate_shell_block() {
    // Symmetric to doctor_fails_on_duplicate_init_block: a duplicate
    // `# >>> ghost-complete shell integration >>>` block must also Fail.
    // Without this test the symmetric check at the SHELL_BEGIN side could
    // silently regress.
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let zshrc = home.join(".zshrc");
    let init_block = "# >>> ghost-complete initialize >>>\n\
                      source '/home/user/init.zsh'\n\
                      # <<< ghost-complete initialize <<<\n";
    let shell_block = "# >>> ghost-complete shell integration >>>\n\
                       source '/home/user/ghost-complete.zsh'\n\
                       # <<< ghost-complete shell integration <<<\n";
    std::fs::write(&zshrc, format!("{init_block}{shell_block}{shell_block}")).unwrap();

    let cfg = home.join("config.toml");
    std::fs::write(&cfg, "").unwrap();

    let output = Command::new(ghost_bin())
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
    assert_eq!(
        output.status.code(),
        Some(1),
        "doctor must exit 1 on duplicate shell-integration blocks.\n\
         combined:\n{combined}",
    );
    assert!(
        combined.contains("[FAIL]"),
        "doctor must emit [FAIL] for duplicate shell-integration blocks; \
         got:\n{combined}",
    );
    let lower = combined.to_lowercase();
    assert!(
        lower.contains("duplicate"),
        "doctor must surface 'duplicate' in the message; got:\n{combined}",
    );
    // The error wording should name which block(s) are duplicated.
    assert!(
        lower.contains("shell=") || lower.contains("shell ") || lower.contains("shell-integration"),
        "doctor must identify the duplicated shell block; got:\n{combined}",
    );
}

#[test]
fn doctor_warns_on_drifted_shell_integration_content() {
    // A user with both shell-integration files on disk at the canonical
    // path, but with content drifted from the embedded copy by a single
    // byte. The doctor must Warn (exit 0) with a "drifted" message so the
    // operator notices and can re-run install.
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
    let shell_dir = home.join(".config/ghost-complete/shell");
    std::fs::create_dir_all(&shell_dir).unwrap();
    let init_path = shell_dir.join("init.zsh");
    let script_path = shell_dir.join("ghost-complete.zsh");
    // Drift by appending one extra line. The OSC 7770 legacy check must
    // not match (neither 7770; nor 7772; is added), so this falls through
    // to the content-drift Warn.
    std::fs::write(&init_path, format!("{zsh_init}# drifted by one line\n")).unwrap();
    std::fs::write(&script_path, &zsh_integration).unwrap();

    let zshrc = home.join(".zshrc");
    let zshrc_contents = format!(
        "# >>> ghost-complete initialize >>>\n\
         builtin source '{init}'\n\
         # <<< ghost-complete initialize <<<\n\
         # >>> ghost-complete shell integration >>>\n\
         source '{script}'\n\
         # <<< ghost-complete shell integration <<<\n",
        init = init_path.display(),
        script = script_path.display(),
    );
    std::fs::write(&zshrc, &zshrc_contents).unwrap();

    let cfg = home.join("config.toml");
    std::fs::write(&cfg, "").unwrap();

    let output = Command::new(ghost_bin())
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
        output.status.success(),
        "content drift must be Warn, not Fail.\nexit: {:?}\ncombined:\n{combined}",
        output.status.code(),
    );
    assert!(
        combined.contains("[WARN]"),
        "doctor must emit [WARN] for drifted shell integration content; \
         got:\n{combined}",
    );
    assert!(
        combined.to_lowercase().contains("drifted"),
        "doctor must surface 'drifted' in the message; got:\n{combined}",
    );
}

#[test]
fn doctor_warns_on_legacy_osc_7770_reporter() {
    // A user upgrading from a pre-OSC-7772 install (where ghost-complete.zsh
    // emitted `\e]7770;...` instead of `\e]7772;...`). The doctor must
    // detect the legacy reporter and surface a migration-specific Warn
    // even though the file content drifts from the embedded copy (the
    // OSC 7770 check runs before the drift check so the user gets a
    // pointed migration message instead of the generic drift Warn).
    use tempfile::TempDir;

    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root");
    let zsh_init = std::fs::read_to_string(repo_root.join("shell/init.zsh"))
        .expect("read shell/init.zsh from repo");

    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let shell_dir = home.join(".config/ghost-complete/shell");
    std::fs::create_dir_all(&shell_dir).unwrap();
    let init_path = shell_dir.join("init.zsh");
    let script_path = shell_dir.join("ghost-complete.zsh");
    std::fs::write(&init_path, &zsh_init).unwrap();
    // Synthetic pre-OSC-7772 ghost-complete.zsh — emits 7770;, never 7772;.
    let legacy_script = "#!/usr/bin/env zsh\n\
                         _gc_legacy_report() {\n  \
                         printf '\\e]7770;1;%s\\a' \"$BUFFER\"\n\
                         }\n";
    std::fs::write(&script_path, legacy_script).unwrap();

    let zshrc = home.join(".zshrc");
    let zshrc_contents = format!(
        "# >>> ghost-complete initialize >>>\n\
         builtin source '{init}'\n\
         # <<< ghost-complete initialize <<<\n\
         # >>> ghost-complete shell integration >>>\n\
         source '{script}'\n\
         # <<< ghost-complete shell integration <<<\n",
        init = init_path.display(),
        script = script_path.display(),
    );
    std::fs::write(&zshrc, &zshrc_contents).unwrap();

    let cfg = home.join("config.toml");
    std::fs::write(&cfg, "").unwrap();

    let output = Command::new(ghost_bin())
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
        output.status.success(),
        "legacy OSC 7770 reporter must be Warn, not Fail.\n\
         exit: {:?}\ncombined:\n{combined}",
        output.status.code(),
    );
    assert!(
        combined.contains("[WARN]"),
        "doctor must emit [WARN] for legacy OSC 7770 reporter; got:\n{combined}",
    );
    assert!(
        combined.contains("7770") && combined.contains("7772"),
        "doctor must reference both OSC numbers so the user understands the \
         migration; got:\n{combined}",
    );
}

#[test]
fn doctor_fails_when_one_block_has_no_parseable_source() {
    // Hand-edit corruption: the init block keeps a normal `source` line,
    // but the shell-integration block has the source line removed (only the
    // marker comments remain). Pre-fix this would have been bucketed under
    // the same `(None, None) => pre-v0.9 Warn` path as a fully unsourced
    // install; the post-fix doctor distinguishes "one block missing source"
    // as hand-edit corruption and Fails.
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let zshrc = home.join(".zshrc");
    let zshrc_contents = "# >>> ghost-complete initialize >>>\n\
         builtin source '/home/user/init.zsh'\n\
         # <<< ghost-complete initialize <<<\n\
         # >>> ghost-complete shell integration >>>\n\
         # (source line removed by a botched manual edit)\n\
         # <<< ghost-complete shell integration <<<\n";
    std::fs::write(&zshrc, zshrc_contents).unwrap();

    let cfg = home.join("config.toml");
    std::fs::write(&cfg, "").unwrap();

    let output = Command::new(ghost_bin())
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
    assert_eq!(
        output.status.code(),
        Some(1),
        "doctor must exit 1 when exactly one block lacks a parseable source.\n\
         combined:\n{combined}",
    );
    assert!(
        combined.contains("[FAIL]"),
        "doctor must emit [FAIL] for hand-edit corruption; got:\n{combined}",
    );
    assert!(
        combined.contains("no parseable"),
        "doctor must surface 'no parseable' in the message; got:\n{combined}",
    );
}
