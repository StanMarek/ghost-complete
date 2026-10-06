//! `init.zsh` must not start a proxy inside a proxy. It recognises its
//! parent proxy by the process name `ps -o comm` reports, which on macOS is
//! argv0. A terminal that launches the proxy directly (terminal-launched
//! mode, #186) may start it the way login(1) starts a shell, with a leading
//! dash (`-ghost-complete`, Rio before 0.5.8; Ghostty with a command that is
//! not an absolute path), or by its absolute path. All of these are the
//! proxy; any other process is not, and then a set `GHOST_COMPLETE_ACTIVE`
//! leaked in from elsewhere.
//!
//! Runs the real `init.zsh` under zsh, as the child of a process whose argv0
//! is set per case, with a fake `ghost-complete` on PATH that only announces
//! it was started. That parent is orphaned first, so the ancestor walk ends
//! at it and never reaches the process tree running the tests, which may
//! itself sit under a real ghost-complete.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PROXY_STARTED: &str = "PROXY_STARTED\n";
const SHELL_KEPT: &str = "SHELL_KEPT\n";

/// Sources the `init.zsh` given as `$1`, then reports, if it didn't exec.
const SHELL_SCRIPT: &str = r#"source "$1"
print -r -- SHELL_KEPT
"#;

/// The parent: waits until launchd adopts it, runs the shell as its child
/// (`$1` script, `$2` init.zsh), and records the output in `$3`, then
/// signals completion through `$4`.
const PARENT_SCRIPT: &str = r#"until [[ ${$(ps -o ppid= -p $$)// /} == 1 ]]; do sleep 0.01; done
zsh -f "$1" "$2" > "$3"
print -r -- $? > "$4"
"#;

/// Starts the parent (`$2` with args `$3`..) with argv0 `$1` from a subshell
/// that exits at once, orphaning it.
const LAUNCHER: &str = r#"( exec -a "$1" /bin/zsh -f "${@:2}" & )"#;

#[derive(Clone, Copy, Debug)]
enum Branch {
    Direct,
    Tmux,
}

/// Sources `init.zsh` in a zsh whose only ancestor runs with argv0
/// `parent_argv0`.
fn source_init_zsh_under(branch: Branch, parent_argv0: &str) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let fake = bin.join("ghost-complete");
    std::fs::write(&fake, format!("#!/bin/sh\nprintf '{PROXY_STARTED}'\n")).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let script = tmp.path().join("shell.zsh");
    std::fs::write(&script, SHELL_SCRIPT).unwrap();
    let parent = tmp.path().join("parent.zsh");
    std::fs::write(&parent, PARENT_SCRIPT).unwrap();
    let out = tmp.path().join("out");
    let done = tmp.path().join("done");
    let init_zsh = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../shell/init.zsh");

    let mut cmd = Command::new("/bin/zsh");
    cmd.args(["-f", "-c", LAUNCHER, "launcher", parent_argv0])
        .arg(&parent)
        .arg(&script)
        .arg(&init_zsh)
        .arg(&out)
        .arg(&done)
        .env_clear()
        .env("HOME", tmp.path())
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("TERM_PROGRAM", "ghostty")
        .env("GHOST_COMPLETE_ACTIVE", "1")
        .stdin(Stdio::null());
    if let Branch::Tmux = branch {
        cmd.env("TMUX", "/tmp/tmux-test/default,1,0")
            .env("TMUX_PANE", "%1")
            .env("GHOSTTY_RESOURCES_DIR", tmp.path());
    }
    let status = cmd.status().expect("run launcher");
    assert!(status.success(), "launcher failed");

    let deadline = Instant::now() + Duration::from_secs(20);
    while !done.exists() {
        assert!(Instant::now() < deadline, "orphaned parent never finished");
        std::thread::sleep(Duration::from_millis(20));
    }
    // `done` is written after `out` is closed; give the write a moment to land.
    while std::fs::read_to_string(&done)
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        assert!(Instant::now() < deadline, "exit status never written");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        std::fs::read_to_string(&done).unwrap().trim(),
        "0",
        "shell exited non-zero"
    );
    std::fs::read_to_string(&out).unwrap()
}

#[test]
fn proxy_parent_is_recognised_however_it_was_launched() {
    for branch in [Branch::Direct, Branch::Tmux] {
        for argv0 in [
            "ghost-complete",
            "/opt/homebrew/bin/ghost-complete",
            "-ghost-complete",
            "-/opt/homebrew/bin/ghost-complete",
        ] {
            assert_eq!(
                source_init_zsh_under(branch, argv0),
                SHELL_KEPT,
                "{branch:?} parent argv0 {argv0:?}"
            );
        }
    }
}

/// Unchanged: a parent that is not the proxy means `GHOST_COMPLETE_ACTIVE`
/// leaked in from another shell (an editor started from one), so the proxy
/// starts.
#[test]
fn other_parent_starts_the_proxy() {
    for branch in [Branch::Direct, Branch::Tmux] {
        for argv0 in ["zsh", "-zsh", "/bin/zsh", "ghost-complete-helper"] {
            assert_eq!(
                source_init_zsh_under(branch, argv0),
                PROXY_STARTED,
                "{branch:?} parent argv0 {argv0:?}"
            );
        }
    }
}
