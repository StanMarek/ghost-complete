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

/// Pins clap's `ValueEnum` rejection of typo'd `--log-level` values at the
/// parse boundary. The `LogLevel` enum was introduced specifically so a
/// typo like `--log-level deubg` errors out instead of silently being
/// rewritten to `warn` inside `init_tracing`. If a future refactor reverted
/// the field to `log_level: String` (or dropped `value_enum` /
/// `default_value_t`), the rejection would silently disappear and the
/// silent-rewrite-to-warn behavior would return — this test fails loudly
/// in that scenario.
#[test]
fn invalid_log_level_rejected_at_parse_time() {
    let tmp = isolated_home();
    let output = cmd_with_isolated_home(tmp.path())
        .arg("--log-level")
        .arg("deubg")
        .arg("status")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "typo'd --log-level must error at parse time; got success.\n\
         stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--log-level") && stderr.contains("invalid value"),
        "expected clap ValueEnum rejection mentioning `--log-level` and \
         `invalid value`. If this regressed, the `LogLevel` enum was likely \
         reverted to a free-form `String`, restoring the silent-rewrite-to-warn \
         fallback in init_tracing.\nstderr:\n{stderr}"
    );
    // Sanity check that clap is suggesting one of the legal values — pins
    // the `[possible values: ...]` rendering that ValueEnum produces.
    assert!(
        stderr.contains("possible values") || stderr.contains("debug"),
        "expected clap to surface `possible values` or a typo suggestion. \
         Its absence suggests the ValueEnum derive was dropped.\n\
         stderr:\n{stderr}"
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

/// End-to-end regression guard: `status --json` must route through the JSON
/// formatter (`run_status_json`) rather than the text formatter. The sibling
/// `strict` flag is already covered by `status_strict_flag_propagates_from_cli`;
/// this is the symmetric `json` flag test. A refactor that transposed
/// `(strict, json)` or hard-coded `json=false` during dispatch in `main.rs`
/// would silently regress users to the text formatter — but both existing
/// strict tests would still pass. Parsing stdout as a JSON object and pinning
/// the `schema_version` key catches that regression.
#[test]
fn status_json_flag_propagates_from_cli() {
    let tmp = isolated_home();
    let spec_dir = tmp.path().join("specs");
    std::fs::create_dir_all(&spec_dir).unwrap();
    let cfg = write_config_with_spec_dir(&tmp, &spec_dir);

    let output = cmd_with_isolated_home(tmp.path())
        .arg("--config")
        .arg(&cfg)
        .arg("status")
        .arg("--json")
        .output()
        .unwrap();

    assert_success(&output, "status --json");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The JSON formatter emits a single pretty-printed JSON object followed
    // by a trailing newline. If `--json` did not reach the handler, the text
    // formatter would have run and stdout would not parse as JSON.
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "`status --json` stdout must parse as a JSON object — the `json` \
             flag was dropped during routing if this fails. parse error: {e}\n\
             stdout:\n{stdout}"
        )
    });
    assert!(
        parsed.is_object(),
        "expected a JSON object at the top level, got: {parsed}"
    );
    assert!(
        parsed.get("schema_version").is_some(),
        "expected `schema_version` key from run_status_json — its absence \
         means the text formatter ran instead of the JSON formatter.\n\
         parsed:\n{parsed}"
    );
    // Pin a second well-known key for defense-in-depth: a malformed
    // alternative formatter producing `{}` would otherwise satisfy the
    // `schema_version` check above only after a deliberate sabotage.
    assert!(
        parsed.get("spec_counts").is_some(),
        "expected `spec_counts` key from run_status_json — its absence \
         suggests the JSON shape regressed or a different formatter ran.\n\
         parsed:\n{parsed}"
    );
}

/// End-to-end regression guard: `validate-specs --json` must route through
/// the NDJSON formatter rather than the text formatter. Mirrors
/// `status_json_flag_propagates_from_cli` for the sibling subcommand. A
/// refactor that hard-coded `json=false` or transposed `(strict, json)` in
/// `main.rs` dispatch would silently regress users to the text formatter —
/// the existing strict tests would still pass.
///
/// The validate output is NDJSON (one JSON object per line) — one row per
/// spec plus a trailing `{"summary":{...}}` row.
#[test]
fn validate_specs_json_flag_propagates_from_cli() {
    let tmp = isolated_home();
    let spec_dir = tmp.path().join("specs");
    std::fs::create_dir_all(&spec_dir).unwrap();
    // A trivially well-formed spec produces one `{"spec_name":..., "ok":true}`
    // row in NDJSON mode, plus the trailing summary row.
    std::fs::write(spec_dir.join("ok.json"), r#"{"name":"ok"}"#).unwrap();
    let cfg = write_config_with_spec_dir(&tmp, &spec_dir);

    let output = cmd_with_isolated_home(tmp.path())
        .arg("--config")
        .arg(&cfg)
        .arg("validate-specs")
        .arg("--json")
        .output()
        .unwrap();

    assert_success(&output, "validate-specs --json");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // NDJSON: split on lines, skip empties, parse each as a JSON object.
    let rows: Vec<serde_json::Value> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            serde_json::from_str(l).unwrap_or_else(|e| {
                panic!(
                    "`validate-specs --json` line must parse as JSON — the \
                     `json` flag was dropped during routing if this fails. \
                     line: {l:?}\nparse error: {e}\nstdout:\n{stdout}"
                )
            })
        })
        .collect();

    assert!(
        !rows.is_empty(),
        "expected at least one NDJSON row from validate-specs --json; got \
         empty stdout. The `json` flag likely did not reach the handler.\n\
         stdout:\n{stdout}"
    );

    // A per-spec row carries `spec_name`; the trailing row carries `summary`.
    let per_spec_rows: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|r| r.get("spec_name").is_some())
        .collect();
    let summary_rows: Vec<&serde_json::Value> =
        rows.iter().filter(|r| r.get("summary").is_some()).collect();

    assert_eq!(
        per_spec_rows.len(),
        1,
        "expected exactly one per-spec NDJSON row for our `ok.json` spec; \
         got {}. rows:\n{rows:#?}",
        per_spec_rows.len()
    );
    assert_eq!(
        summary_rows.len(),
        1,
        "expected exactly one trailing summary NDJSON row; got {}. If this \
         is 0, the text formatter ran instead of the JSON formatter.\n\
         rows:\n{rows:#?}",
        summary_rows.len()
    );

    // Pin the per-spec row shape — proves `emit_json_spec` ran with the
    // expected fields rather than e.g. an arbitrary alternative formatter
    // emitting JSON-shaped but mislabeled output.
    let spec_row = per_spec_rows[0];
    assert_eq!(
        spec_row["ok"],
        serde_json::Value::Bool(true),
        "expected ok=true for a well-formed spec; got row:\n{spec_row}"
    );
    assert!(
        spec_row.get("divergences").is_some() && spec_row.get("warnings").is_some(),
        "expected `divergences` and `warnings` keys on the per-spec row — \
         their absence means the wrong formatter ran.\nrow:\n{spec_row}"
    );

    // Defense-in-depth: the text formatter emits a `Validating specs in ...`
    // banner before any per-spec output. Its absence is an independent
    // signal that the JSON path ran.
    assert!(
        !stdout.contains("Validating specs in"),
        "`validate-specs --json` stdout must NOT contain the text formatter's \
         `Validating specs in` banner. Routing regressed: --json fell through \
         to the text formatter.\nstdout:\n{stdout}"
    );
}

/// End-to-end regression guard for the `--baseline <path>` positive path:
/// the parsed `Option<PathBuf>` must reach `status::run_status_with_opts`
/// as `baseline.as_deref()`. The existing
/// `status_baseline_without_value_fails_at_parse_time` covers the bare
/// `--baseline` negative case at parse time, but no test confirms a value
/// is actually forwarded through clap → `main.rs` dispatch → the status
/// handler. If a regression hard-coded `baseline.as_deref()` to `None`
/// (e.g. a refactor accidentally dropping the destructure binding),
/// `--baseline /missing.json` would silently fall through to the embedded
/// baseline lookup instead of erroring with the authoritative
/// `baseline file does not exist` message at status.rs:166. This test
/// pins that contract end-to-end.
#[test]
fn status_baseline_path_propagates_from_cli() {
    let tmp = isolated_home();
    let missing_baseline = tmp.path().join("nonexistent-baseline.json");
    assert!(
        !missing_baseline.exists(),
        "preconditions: baseline path must not exist for this test to be meaningful"
    );

    let output = cmd_with_isolated_home(tmp.path())
        .arg("status")
        .arg("--baseline")
        .arg(&missing_baseline)
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "`status --baseline <missing>` must exit non-zero — load_baseline \
         must surface the missing path as an error. If this exited 0, the \
         parsed PathBuf likely never reached run_status_with_opts \
         (`baseline.as_deref()` was dropped or hard-coded to None).\n\
         stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("baseline file does not exist"),
        "expected the authoritative `baseline file does not exist` bail \
         message (status.rs:166) — its absence means the explicit-baseline \
         arm of load_baseline was not reached and the CLI value was \
         silently dropped during routing.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Also pin that the error references the path we supplied, ruling out
    // a routing arm that pointed load_baseline at a different (stale) value.
    assert!(
        combined.contains("nonexistent-baseline.json"),
        "expected the error to mention our supplied baseline path. Its \
         absence means the parsed value was dropped or replaced by a \
         different path before reaching load_baseline.\n\
         stdout:\n{stdout}\nstderr:\n{stderr}"
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

    // Positive TUI-arm signal: with stdin=null and stdout=piped, the TUI's
    // `enable_raw_mode()` fails with a recognisable error chain
    // (`failed to enable raw mode\n\nCaused by:\n    Device not
    // configured`). Pinning this stderr fingerprint closes the gap where a
    // refactor could re-route `Config { subcommand: Some(Edit) }` to an
    // arbitrary wrong arm (e.g. `doctor::run_doctor`) whose non-zero exit
    // and absence of dump markers would otherwise pass silently.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("raw mode") || stderr.contains("raw_mode"),
        "expected TUI-init failure signature (`raw mode`) in stderr — its \
         absence suggests `config edit` did not route to the TUI arm.\n\
         stderr:\n{stderr}"
    );
}

/// Verifies the documented `--` escape hatch from `after_help`: argv after
/// `--` must route through the `External(Vec<OsString>)` arm into proxy
/// mode rather than trip a clap-level "unrecognized subcommand" / "unknown
/// argument" error. If clap stopped honouring `--` for `external_subcommand`
/// (or the arm was deleted), this test fails and signals that the
/// `after_help` advice is misleading.
///
/// The positive signal is the `--log-file` log, not the process's
/// stdout/stderr: `run_proxy` records `starting ghost-complete proxy` with
/// `shell=<argv[0]>` before handing off to `gc_pty::run_proxy`, so it is
/// captured no matter where the proxy later bails. Asserting on the
/// spawn-failure *message* instead would be environment dependent —
/// `gc_pty`'s `spawn_shell` queries the terminal size before spawning, and
/// `crossterm`'s size query fails on a headless CI runner (`ioctl` has no
/// tty, `tput` has no `$TERM`), so the proxy bails with `failed to query
/// terminal size` long before it reaches the spawn. Mirrors the log-based
/// signal in `proxy_with_no_args_uses_default_shell_from_env`.
#[test]
fn dash_dash_escape_routes_subcommand_named_shell_to_external() {
    let tmp = isolated_home();
    let log_file = tmp.path().join("escape.log");
    // A path that cannot exist on the filesystem; its final component
    // (`install`) echoes the `after_help` example `ghost-complete -- install`.
    let bogus_shell = "/tmp/ghost-complete-bogus-shell-does-not-exist/install";

    let output = cmd_with_isolated_home(tmp.path())
        .arg("--log-level")
        .arg("info")
        .arg("--log-file")
        .arg(&log_file)
        .arg("--")
        .arg(bogus_shell)
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

    // clap's "unrecognized subcommand" / "unknown argument" / "for more
    // information, try '--help'" wording must NOT appear — those signal the
    // `External` arm did not catch the routing and clap rejected the input
    // at parse time.
    let stderr = String::from_utf8_lossy(&output.stderr);
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

    // Positive signal: the `External` arm reached `run_proxy`, which logs
    // `starting ghost-complete proxy` with `shell=<argv[0]>`. Both strings
    // present proves the `--`-escaped argv was forwarded into proxy mode as
    // the shell rather than being rejected by clap at parse time. Reading
    // the log keeps the assertion independent of the headless-CI
    // terminal-size failure described above.
    let log = std::fs::read_to_string(&log_file)
        .unwrap_or_else(|e| panic!("log file at {} unreadable: {e}", log_file.display()));
    assert!(
        log.contains("starting ghost-complete proxy"),
        "log must record the proxy startup line — proves the `--` escape \
         routed into the External arm and reached run_proxy.\nlog:\n{log}"
    );
    assert!(
        log.contains(bogus_shell),
        "log must record `shell={bogus_shell}` — proves the `--`-escaped \
         argv[0] became the proxy's shell.\nlog:\n{log}"
    );
}

/// Pins the `None => run_proxy(..., Vec::new())` routing arm in `main.rs`.
/// Invokes ghost-complete with NO positional argv and no subcommand. The
/// proxy reads `$SHELL` via `resolve_default_shell()`, logs `"starting
/// ghost-complete proxy"` with that shell, then bails when `enable_raw_mode`
/// fails (stdin is routed through `Stdio::null()` and stdout/stderr through
/// `Stdio::piped()` — none of them is a TTY, so `enable_raw_mode` fails
/// fast). The log file is the deterministic signal: if the `None` arm
/// regressed (e.g. swapped with `External`, hard-coded a different shell,
/// or panicked), the recorded `shell=` line would not match the $SHELL
/// value we set — or the log would be empty because tracing never
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
