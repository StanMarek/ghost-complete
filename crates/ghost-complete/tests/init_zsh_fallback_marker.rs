//! When the proxy can't start it execs the user's shell in its place, marked
//! with `GHOST_COMPLETE_FALLBACK_PID` (#186). That shell sources `.zshrc`
//! again; `init.zsh` must recognise it and not start the proxy, or the two
//! start each other forever. The same goes for a zsh that shell starts
//! before `.zshrc` runs (a `$SHELL` wrapper script, or a bash whose `.bashrc`
//! runs zsh): it is a child, not the same process. The marker must not stop
//! any shell outside that chain.
//!
//! Runs the real `init.zsh` under zsh, with a fake `ghost-complete` on PATH
//! that only announces it was started.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

const PROXY_STARTED: &str = "PROXY_STARTED\n";
const SHELL_KEPT: &str = "SHELL_KEPT marker=unset\n";

/// Sources the `init.zsh` given as `$1`, then reports, if it didn't exec.
const SHELL_SCRIPT: &str = r#"source "$1"
print -r -- "SHELL_KEPT marker=${GHOST_COMPLETE_FALLBACK_PID-unset}"
"#;

/// Run `"$@"` with the marker set to this process's pid, as the proxy's
/// fallback does.
const MARK_AND_EXEC: &str = r#"export GHOST_COMPLETE_FALLBACK_PID=$$; exec "$@""#;
/// The same, but `"$@"` runs as a child.
const MARK_AND_FORK: &str = r#"export GHOST_COMPLETE_FALLBACK_PID=$$; "$@"; exit $?"#;
/// Run `"$@"` as a child.
const FORK: &str = r#""$@"; exit $?"#;

#[derive(Clone, Copy)]
enum Branch {
    Direct,
    Tmux,
}

/// Which process the marker names, relative to the zsh sourcing `init.zsh`.
#[derive(Clone, Copy)]
enum Marker {
    /// The fallback shell itself (`exec` kept the proxy's pid).
    ThisShell,
    /// The fallback shell is the parent, e.g. a `$SHELL` wrapper script.
    Parent,
    Grandparent,
    /// A live process outside this shell's ancestry.
    Unrelated,
}

fn source_init_zsh(branch: Branch, marker: Marker) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let fake = bin.join("ghost-complete");
    std::fs::write(&fake, format!("#!/bin/sh\nprintf '{PROXY_STARTED}'\n")).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let script = tmp.path().join("shell.zsh");
    std::fs::write(&script, SHELL_SCRIPT).unwrap();
    let init_zsh = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../shell/init.zsh");

    let zsh = [
        "zsh".as_ref(),
        "-f".as_ref(),
        script.as_os_str(),
        init_zsh.as_os_str(),
    ];
    let sh = |body: &str| vec!["/bin/sh".into(), "-c".into(), body.to_owned(), "sh".into()];
    let mut unrelated = None;
    let mut argv: Vec<std::ffi::OsString> = match marker {
        Marker::ThisShell => sh(MARK_AND_EXEC).into_iter().map(Into::into).collect(),
        Marker::Parent => sh(MARK_AND_FORK).into_iter().map(Into::into).collect(),
        Marker::Grandparent => sh(MARK_AND_FORK)
            .into_iter()
            .chain(sh(FORK))
            .map(Into::into)
            .collect(),
        Marker::Unrelated => {
            let sleeper = Command::new("sleep").arg("30").spawn().unwrap();
            let pid = sleeper.id().to_string();
            unrelated = Some(sleeper);
            vec![
                "/usr/bin/env".into(),
                format!("GHOST_COMPLETE_FALLBACK_PID={pid}").into(),
            ]
        }
    };
    argv.extend(zsh.iter().map(Into::into));

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .env_clear()
        .env("HOME", tmp.path())
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("TERM_PROGRAM", "ghostty")
        .stdin(Stdio::null());
    if let Branch::Tmux = branch {
        cmd.env("TMUX", "/tmp/tmux-test/default,1,0")
            .env("TMUX_PANE", "%1")
            .env("GHOSTTY_RESOURCES_DIR", tmp.path());
    }
    let output = cmd.output().expect("run zsh");
    if let Some(mut sleeper) = unrelated {
        let _ = sleeper.kill();
        let _ = sleeper.wait();
    }
    assert!(
        output.status.success(),
        "zsh failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn fallback_shell_does_not_restart_the_proxy() {
    assert_eq!(
        source_init_zsh(Branch::Direct, Marker::ThisShell),
        SHELL_KEPT
    );
}

#[test]
fn fallback_shell_in_tmux_does_not_restart_the_proxy() {
    assert_eq!(source_init_zsh(Branch::Tmux, Marker::ThisShell), SHELL_KEPT);
}

#[test]
fn child_of_fallback_shell_does_not_restart_the_proxy() {
    assert_eq!(source_init_zsh(Branch::Direct, Marker::Parent), SHELL_KEPT);
}

#[test]
fn grandchild_of_fallback_shell_does_not_restart_the_proxy() {
    assert_eq!(
        source_init_zsh(Branch::Direct, Marker::Grandparent),
        SHELL_KEPT
    );
}

/// A marker inherited from outside this shell's ancestry (a tmux server, an
/// editor launched from a fallback shell) must not stop the proxy.
#[test]
fn unrelated_marker_does_not_block_the_proxy() {
    assert_eq!(
        source_init_zsh(Branch::Direct, Marker::Unrelated),
        PROXY_STARTED
    );
}

#[test]
fn unrelated_marker_does_not_block_the_proxy_in_tmux() {
    assert_eq!(
        source_init_zsh(Branch::Tmux, Marker::Unrelated),
        PROXY_STARTED
    );
}
