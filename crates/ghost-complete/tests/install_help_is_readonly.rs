//! Regression tests pinning read-only invocations on write-capable commands.
//!
//! `install --help` and `install --dry-run` must never mutate the caller's
//! HOME. These tests run with an isolated HOME so a regression cannot touch
//! the caller's real shell files.

use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

fn ghost_bin() -> PathBuf {
    env!("CARGO_BIN_EXE_ghost-complete").into()
}

fn command_with_isolated_home(home: &std::path::Path) -> Command {
    let mut cmd = Command::new(ghost_bin());
    cmd.env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME");
    cmd
}

#[test]
fn install_help_does_not_write_zshrc() {
    let tmp = TempDir::new().expect("tempdir");
    let home = tmp.path();

    let output = command_with_isolated_home(home)
        .arg("install")
        .arg("--help")
        .output()
        .expect("spawn ghost-complete");

    assert!(
        output.status.success(),
        "install --help should exit 0; got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("install") && stdout.to_lowercase().contains("help"),
        "expected install help text; got:\n{stdout}",
    );

    assert!(
        !home.join(".zshrc").exists(),
        "install --help must NOT create ~/.zshrc",
    );
    assert!(
        !home.join(".config/ghost-complete").exists(),
        "install --help must NOT create ~/.config/ghost-complete/",
    );
    assert!(
        !home.join(".backup.ghost-complete").exists(),
        "install --help must NOT create ~/.backup.ghost-complete",
    );
}

#[test]
fn uninstall_help_does_not_write_zshrc() {
    let tmp = TempDir::new().expect("tempdir");
    let home = tmp.path();

    let output = command_with_isolated_home(home)
        .arg("uninstall")
        .arg("--help")
        .output()
        .expect("spawn ghost-complete");

    assert!(
        output.status.success(),
        "uninstall --help should exit 0; got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        !home.join(".zshrc").exists(),
        "uninstall --help must NOT create ~/.zshrc",
    );
    assert!(
        !home.join(".config/ghost-complete").exists(),
        "uninstall --help must NOT create ~/.config/ghost-complete/",
    );
    assert!(
        !home.join(".backup.ghost-complete").exists(),
        "uninstall --help must NOT create ~/.backup.ghost-complete",
    );
}

#[test]
fn install_dry_run_through_clap_does_not_write() {
    let tmp = TempDir::new().expect("tempdir");
    let home = tmp.path();

    let output = command_with_isolated_home(home)
        .arg("install")
        .arg("--dry-run")
        .output()
        .expect("spawn ghost-complete");

    assert!(
        output.status.success(),
        "install --dry-run should exit 0; got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Dry run:"),
        "expected dry-run banner in stdout; got:\n{stdout}",
    );

    assert!(
        !home.join(".zshrc").exists(),
        "install --dry-run must NOT create ~/.zshrc",
    );
    assert!(
        !home.join(".config/ghost-complete/shell").exists(),
        "install --dry-run must NOT create ~/.config/ghost-complete/shell",
    );
    assert!(
        !home.join(".config/ghost-complete/specs").exists(),
        "install --dry-run must NOT create ~/.config/ghost-complete/specs",
    );
    assert!(
        !home.join(".config/ghost-complete/config.toml").exists(),
        "install --dry-run must NOT create ~/.config/ghost-complete/config.toml",
    );
}
