//! CLI parsing and routing coverage for real clap subcommands.

#[allow(dead_code)]
mod harness;

use std::path::{Path, PathBuf};
use std::process::Command;

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
    let output = cmd_with_isolated_home(tmp.path()).arg("--help").output().unwrap();

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
