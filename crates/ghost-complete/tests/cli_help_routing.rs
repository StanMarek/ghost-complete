//! CLI parsing and routing coverage for real clap subcommands.

#[allow(dead_code)]
mod harness;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use harness::GhostProcess;
use tempfile::TempDir;

fn ghost_bin() -> PathBuf {
    env!("CARGO_BIN_EXE_ghost-complete").into()
}

fn isolated_home() -> TempDir {
    TempDir::new().expect("tempdir")
}

fn cmd_with_isolated_home(home: &Path) -> Command {
    let mut cmd = Command::new(ghost_bin());
    cmd.env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME");
    cmd
}

fn assert_success(output: &std::process::Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn top_level_help_lists_real_subcommands() {
    let tmp = isolated_home();
    let output = cmd_with_isolated_home(tmp.path())
        .arg("--help")
        .output()
        .unwrap();

    assert_success(&output, "top-level --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for subcommand in [
        "install",
        "uninstall",
        "status",
        "validate-specs",
        "config",
        "doctor",
    ] {
        assert!(
            stdout.contains(subcommand),
            "top-level --help missing {subcommand}; got:\n{stdout}",
        );
    }
}

#[test]
fn real_subcommand_help_exits_zero_and_lists_flags() {
    let cases: &[(&[&str], &[&str])] = &[
        (&["install"], &["--dry-run"]),
        (&["uninstall"], &[]),
        (&["status"], &["--strict", "--json", "--baseline"]),
        (&["validate-specs"], &["--strict", "--json"]),
        (&["config"], &["edit"]),
        (&["config", "edit"], &[]),
        (&["doctor"], &[]),
    ];

    for (argv, expected) in cases {
        let tmp = isolated_home();
        let output = cmd_with_isolated_home(tmp.path())
            .args(*argv)
            .arg("--help")
            .output()
            .unwrap();

        assert_success(&output, &format!("{argv:?} --help"));
        let stdout = String::from_utf8_lossy(&output.stdout);
        for needle in *expected {
            assert!(
                stdout.contains(needle),
                "{argv:?} --help missing {needle}; got:\n{stdout}",
            );
        }
    }
}

#[test]
fn status_baseline_without_value_fails_at_parse_time() {
    let tmp = isolated_home();
    let output = cmd_with_isolated_home(tmp.path())
        .arg("status")
        .arg("--baseline")
        .output()
        .unwrap();

    assert!(!output.status.success(), "bare --baseline must error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--baseline") && (stderr.contains("value") || stderr.contains("required")),
        "expected clap missing-value error; got:\n{stderr}",
    );
}

#[test]
fn unknown_flag_on_real_subcommand_fails() {
    let tmp = isolated_home();
    let output = cmd_with_isolated_home(tmp.path())
        .arg("status")
        .arg("--this-flag-does-not-exist")
        .output()
        .unwrap();

    assert!(!output.status.success(), "unknown flag must error");
}

#[test]
fn globals_parse_before_each_real_subcommand() {
    let cases: &[&[&str]] = &[
        &["install"],
        &["uninstall"],
        &["status"],
        &["validate-specs"],
        &["config"],
        &["config", "edit"],
        &["doctor"],
    ];

    for argv in cases {
        let tmp = isolated_home();
        let log_file = tmp.path().join("gc.log");
        let output = cmd_with_isolated_home(tmp.path())
            .arg("--config")
            .arg("/nonexistent/ghost-complete.toml")
            .arg("--log-level")
            .arg("debug")
            .arg("--log-file")
            .arg(&log_file)
            .args(*argv)
            .arg("--help")
            .output()
            .unwrap();

        assert_success(&output, &format!("globals before {argv:?}"));
    }
}

#[test]
fn globals_parse_after_each_real_subcommand() {
    let cases: &[&[&str]] = &[
        &["install"],
        &["uninstall"],
        &["status"],
        &["validate-specs"],
        &["config"],
        &["config", "edit"],
        &["doctor"],
    ];

    for argv in cases {
        let tmp = isolated_home();
        let log_file = tmp.path().join("gc.log");
        let output = cmd_with_isolated_home(tmp.path())
            .args(*argv)
            .arg("--config")
            .arg("/nonexistent/ghost-complete.toml")
            .arg("--log-level")
            .arg("debug")
            .arg("--log-file")
            .arg(&log_file)
            .arg("--help")
            .output()
            .unwrap();

        assert_success(&output, &format!("globals after {argv:?}"));
    }
}

#[test]
fn external_subcommand_falls_back_to_proxy_mode() {
    let mut proc = GhostProcess::spawn();
    proc.send_line("echo proxy-works");
    proc.expect_output("proxy-works");
    let code = proc.exit_with_code(0);
    assert_eq!(code, 0, "expected proxy fallback shell to exit 0");
}

/// Write `config.toml` pointing at a single spec dir; return the cfg path.
fn write_config_with_spec_dir(tmp: &TempDir, spec_dir: &Path) -> PathBuf {
    let cfg = tmp.path().join("config.toml");
    let body = format!(
        "[paths]\nspec_dirs = [\"{}\"]\n",
        spec_dir.display().to_string().replace('\\', "\\\\")
    );
    std::fs::write(&cfg, body).expect("write config.toml");
    cfg
}

/// End-to-end regression guard: `validate-specs --strict` must flip the
/// process exit code to non-zero when a generator warning is surfaced. The
/// `run_validate_specs_inner` unit tests assert the inner logic in isolation;
/// this test instead drives clap → `main.rs` routing → `run_validate_specs_with_opts`
/// so that a future refactor which transposed `strict`/`json`, hard-coded
/// `strict=false`, or dropped the flag during dispatch could not land
/// silently.
#[test]
fn validate_specs_strict_flag_propagates_from_cli() {
    let tmp = isolated_home();
    let spec_dir = tmp.path().join("specs");
    std::fs::create_dir_all(&spec_dir).unwrap();
    // A spec with a doubled split transform — `validate_spec_generators`
    // surfaces this as a warning but parsing succeeds, so non-strict exits
    // 0 and strict exits 1.
    std::fs::write(
        spec_dir.join("bad.json"),
        r#"{
            "name": "bad",
            "args": [{
                "name": "x",
                "generators": [
                    {"script": ["cmd"], "transforms": ["split_lines", "split_lines"]}
                ]
            }]
        }"#,
    )
    .unwrap();
    let cfg = write_config_with_spec_dir(&tmp, &spec_dir);

    // Non-strict: warnings present but exit code is 0.
    let nonstrict = cmd_with_isolated_home(tmp.path())
        .arg("--config")
        .arg(&cfg)
        .arg("validate-specs")
        .output()
        .unwrap();
    assert!(
        nonstrict.status.success(),
        "non-strict validate-specs must exit 0 even when warnings present.\n\
         stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&nonstrict.stdout),
        String::from_utf8_lossy(&nonstrict.stderr),
    );

    // Strict: warnings must promote to non-zero exit.
    let strict = cmd_with_isolated_home(tmp.path())
        .arg("--config")
        .arg(&cfg)
        .arg("validate-specs")
        .arg("--strict")
        .output()
        .unwrap();
    assert!(
        !strict.status.success(),
        "validate-specs --strict must exit non-zero when warnings present, \
         but got success exit. If this regressed, the clap-to-handler routing \
         likely dropped or transposed the `strict` flag.\n\
         stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&strict.stdout),
        String::from_utf8_lossy(&strict.stderr),
    );
    let stdout = String::from_utf8_lossy(&strict.stdout);
    assert!(
        stdout.contains("strict mode"),
        "expected strict-mode banner in stdout to confirm strict path executed:\n{stdout}"
    );
}

/// End-to-end regression guard: `status --strict` must flip the process
/// exit code to non-zero when a spec file in the configured dir fails to
/// parse. Mirrors `validate_specs_strict_flag_propagates_from_cli` for the
/// sibling subcommand so a transposition of strict/json or a refactor that
/// drops the flag during dispatch in `main.rs` cannot land silently.
#[test]
fn status_strict_flag_propagates_from_cli() {
    let tmp = isolated_home();
    let spec_dir = tmp.path().join("specs");
    std::fs::create_dir_all(&spec_dir).unwrap();
    // An obviously broken JSON file — status's lazy parse surfaces it as a
    // parse error, which strict promotes to a non-zero exit.
    std::fs::write(spec_dir.join("broken.json"), "{not valid json").unwrap();
    let cfg = write_config_with_spec_dir(&tmp, &spec_dir);

    // Non-strict: parse errors logged but exit code is 0.
    let nonstrict = cmd_with_isolated_home(tmp.path())
        .arg("--config")
        .arg(&cfg)
        .arg("status")
        .output()
        .unwrap();
    assert!(
        nonstrict.status.success(),
        "non-strict status must exit 0 even when parse errors present.\n\
         stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&nonstrict.stdout),
        String::from_utf8_lossy(&nonstrict.stderr),
    );

    // Strict: parse error must promote to non-zero exit.
    let strict = cmd_with_isolated_home(tmp.path())
        .arg("--config")
        .arg(&cfg)
        .arg("status")
        .arg("--strict")
        .output()
        .unwrap();
    assert!(
        !strict.status.success(),
        "status --strict must exit non-zero on parse errors, but got success. \
         If this regressed, the clap-to-handler routing likely dropped or \
         transposed the `strict` flag.\n\
         stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&strict.stdout),
        String::from_utf8_lossy(&strict.stderr),
    );
    let stdout = String::from_utf8_lossy(&strict.stdout);
    assert!(
        stdout.contains("strict mode"),
        "expected strict-mode banner in stdout to confirm strict path executed:\n{stdout}"
    );
}

/// `config edit` must route to the interactive TUI path, not to the
/// read-only `config` dump path. With stdin forced to `Stdio::null()`,
/// `enable_raw_mode()` (in the TUI entry) fails fast in a non-TTY
/// environment and the process exits non-zero — meanwhile, stdout must NOT
/// contain the TOML-section markers (`[trigger]`, `[popup]`) that the
/// `config` dump emits. If a future refactor accidentally re-routes
/// `config edit` to `config_cmd::run_config` (the dump arm), this test
/// catches it.
#[test]
fn config_edit_attempts_tui_not_dump() {
    let tmp = isolated_home();
    // Use a non-existent config path so that `config` dump would deliberately
    // fall through to the "showing defaults" branch and print recognisable
    // TOML section markers. If `config edit` ever silently fell back to the
    // dump path, those markers would appear on stdout — the assertion
    // below pins that they must not.
    let missing_cfg = tmp.path().join("nonexistent-config.toml");

    let output = cmd_with_isolated_home(tmp.path())
        .arg("--config")
        .arg(&missing_cfg)
        .arg("config")
        .arg("edit")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "`config edit` with stdin=null must fail TUI init (not silently dump). \
         If this exited 0, the routing likely fell through to `run_config`.\n\
         stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The `config` dump fallback emits `# No config file found; showing
    // defaults.` followed by `[trigger]` and `[popup]` section headers.
    // Their presence in stdout means the wrong arm ran.
    assert!(
        !stdout.contains("[trigger]"),
        "`config edit` stdout must NOT contain `[trigger]` (a `config` dump marker). \
         Routing regressed: `Config edit` fell through to the dump path.\n\
         stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("[popup]"),
        "`config edit` stdout must NOT contain `[popup]` (a `config` dump marker). \
         Routing regressed: `Config edit` fell through to the dump path.\n\
         stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("showing defaults"),
        "`config edit` stdout must NOT contain the `config` dump's fallback banner. \
         Routing regressed: `Config edit` fell through to the dump path.\n\
         stdout:\n{stdout}"
    );
}

/// Verifies the documented `--` escape hatch in `after_help` (a user whose
/// shell binary is named like a subcommand can prefix with `--`). The
/// invocation must route through the `External(Vec<String>)` arm and try to
/// spawn the binary — failing with a spawn-error message — rather than
/// produce a clap-level "unrecognized subcommand" / "unknown argument"
/// error. If clap stopped honouring `--` for `external_subcommand` (or the
/// arm was deleted), this test fails and signals that the after_help advice
/// is misleading.
#[test]
fn dash_dash_escape_routes_subcommand_named_shell_to_external() {
    let tmp = isolated_home();
    let output = cmd_with_isolated_home(tmp.path())
        .arg("--")
        .arg("/tmp/ghost-complete-bogus-shell-does-not-exist/install")
        .arg("--some-flag")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "spawning a non-existent shell must fail with non-zero exit.\n\
         stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    // clap's "unrecognized subcommand" / "unknown argument" / "for more
    // information, try '--help'" wording must NOT appear — those signal
    // the `External` arm did not catch the routing and clap rejected the
    // input at parse time.
    let clap_signatures = [
        "unrecognized subcommand",
        "error: unrecognized",
        "for more information, try",
    ];
    for sig in &clap_signatures {
        assert!(
            !stderr.to_lowercase().contains(&sig.to_lowercase()),
            "stderr must not contain clap-level `{sig}` — the `--` escape \
             hatch should route past clap into the External arm.\n\
             stderr:\n{stderr}"
        );
    }
    // Positive signal: the spawn-failure path (the External arm running
    // run_proxy) mentions the bogus path somewhere in the error chain.
    // We accept either stderr or stdout because the proxy may surface the
    // error on either depending on logging config.
    let combined = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), stderr);
    assert!(
        combined.contains("ghost-complete-bogus-shell-does-not-exist")
            || combined.to_lowercase().contains("spawn")
            || combined.to_lowercase().contains("doesn't exist"),
        "expected spawn-failure signature mentioning the bogus binary path; \
         instead got:\n{combined}"
    );
}

/// Pins the `None => run_proxy(..., Vec::new())` routing arm in `main.rs`.
/// Invokes ghost-complete with NO positional argv and no subcommand. The
/// proxy reads `$SHELL` via `resolve_default_shell()`, logs `"starting
/// ghost-complete proxy"` with that shell, then bails when `enable_raw_mode`
/// fails (stdin/stdout are not a TTY because we route them through
/// `Stdio::piped`). The log file is the deterministic signal: if the `None`
/// arm regressed (e.g. swapped with `External`, hard-coded a different
/// shell, or panicked), the recorded `shell=` line would not match the
/// $SHELL value we set — or the log would be empty because tracing never
/// initialised.
///
/// Avoids the PTY harness intentionally — the PTY-backed harness always
/// passes a positional `/bin/sh`, which exercises only the `External(...)`
/// arm. This test fills the gap for the None arm without touching the
/// harness module.
#[test]
fn proxy_with_no_args_uses_default_shell_from_env() {
    let tmp = isolated_home();
    let log_file = tmp.path().join("ghost.log");
    // Pick a recognisable shell path that differs from the host's $SHELL —
    // the assertion below checks this exact string appears in the log so
    // we know the None arm consulted $SHELL rather than a hard-coded
    // fallback.
    let marker_shell = "/tmp/ghost-complete-none-arm-marker-shell";

    let output = cmd_with_isolated_home(tmp.path())
        .env("SHELL", marker_shell)
        .arg("--log-level")
        .arg("info")
        .arg("--log-file")
        .arg(&log_file)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    // Process exits non-zero because raw mode fails outside a TTY; this is
    // expected and indicates the None arm reached `run_proxy`. (A 0 exit
    // here would suggest a different code path ran — for example, a
    // refactor that turned the None arm into a no-op.)
    assert!(
        !output.status.success(),
        "expected non-zero exit from proxy when stdin is not a TTY (raw-mode \
         init fails), got success. The None routing arm likely regressed.\n\
         stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // The log file must record the proxy starting with our marker shell —
    // proving the None arm ran `resolve_default_shell` and called
    // `run_proxy` with the result.
    let log_contents = std::fs::read_to_string(&log_file)
        .unwrap_or_else(|e| panic!("log file at {} unreadable: {e}", log_file.display()));
    assert!(
        log_contents.contains("starting ghost-complete proxy"),
        "log must record the proxy startup line — proves the None arm \
         reached run_proxy.\nlog contents:\n{log_contents}"
    );
    assert!(
        log_contents.contains(marker_shell),
        "log must mention `shell={marker_shell}` — proves the None arm \
         resolved $SHELL via resolve_default_shell rather than using \
         a hard-coded fallback or routing to the wrong arm.\n\
         log contents:\n{log_contents}"
    );
}
