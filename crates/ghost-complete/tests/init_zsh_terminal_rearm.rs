//! A terminal that injects its zsh integration through `ZDOTDIR` (Ghostty,
//! kitty) loses it in the shell behind the proxy: its `.zshenv` ran in the
//! shell that sources `init.zsh` and put the user's `ZDOTDIR` back, and the
//! hook it left for the first prompt never runs, because `init.zsh` execs the
//! proxy first (#186). `init.zsh` must point `ZDOTDIR` at the integration
//! again before the exec, handing the terminal exactly the `ZDOTDIR` the new
//! shell would have inherited anyway.
//!
//! Runs the real `init.zsh` under zsh. The integration directories are
//! fixtures written to the terminals' contract: `.zshenv` restores `ZDOTDIR`
//! from the terminal's variable or unsets it, sources the user's `.zshenv`,
//! and in an interactive shell loads an integration that leaves its hook
//! pending in `precmd_functions`. The real scripts are GPLv3 and stay out of
//! this repository.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Copy, Debug)]
enum Terminal {
    Ghostty,
    Kitty,
}

impl Terminal {
    const ALL: [Terminal; 2] = [Terminal::Ghostty, Terminal::Kitty];

    /// The hook the integration leaves pending until the first prompt.
    fn hook(self) -> &'static str {
        match self {
            Terminal::Ghostty => "_ghostty_deferred_init",
            Terminal::Kitty => "_ksi_deferred_init",
        }
    }

    /// Where the terminal's `.zshenv` finds the user's `ZDOTDIR`.
    fn restore_var(self) -> &'static str {
        match self {
            Terminal::Ghostty => "GHOSTTY_ZSH_ZDOTDIR",
            Terminal::Kitty => "KITTY_ORIG_ZDOTDIR",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Branch {
    Direct,
    Tmux,
}

const BRANCHES: [Branch; 2] = [Branch::Direct, Branch::Tmux];

#[derive(Clone, Copy)]
enum Integration {
    /// Loaded by the terminal; its first-prompt hook is still pending.
    Pending,
    /// Loaded, but its hook is no longer in `precmd_functions`.
    NotPending,
    /// Pending, but the directory has no `.zshenv`.
    NoZshenv,
}

/// The user's `ZDOTDIR` in the shell that sources `init.zsh`.
#[derive(Clone, Copy)]
enum UserZdotdir<'a> {
    Unset,
    Exported(&'a str),
    /// Set by `~/.zshenv` without `export`, so a new shell doesn't inherit it.
    NotExported(&'a str),
}

const ZSHENV: &str = r#"if [[ -n ${RESTORE_VAR+x} ]]; then
  export ZDOTDIR=$RESTORE_VAR
  unset RESTORE_VAR
else
  unset ZDOTDIR
fi
_fixture_zshenv=${ZDOTDIR-$HOME}/.zshenv
[[ -r $_fixture_zshenv ]] && source $_fixture_zshenv
unset _fixture_zshenv
[[ -o interactive ]] && source ${${(%):-%x}:A:h}/integration.zsh
"#;

/// Prints what the proxy would hand to its shell.
const PRINT_ENV_PROXY: &str = r#"#!/bin/sh
printf 'ZDOTDIR=%s GHOSTTY_ZSH_ZDOTDIR=%s KITTY_ORIG_ZDOTDIR=%s\n' \
  "${ZDOTDIR-unset}" "${GHOSTTY_ZSH_ZDOTDIR-unset}" "${KITTY_ORIG_ZDOTDIR-unset}"
"#;

/// Starts an interactive zsh, as the proxy does, and reports what it loaded.
const SPAWN_SHELL_PROXY: &str = r#"#!/bin/sh
exec zsh -i -c '(( ${precmd_functions[(Ie)HOOK]:-0} )) && p=yes || p=no
print -r -- "pending=$p zshenv=${USER_ZSHENV_RAN-no} ZDOTDIR=${ZDOTDIR-unset}"'
"#;

/// `$1`: integration file to load first (may be empty). `$2`: `init.zsh`.
/// `$3`: a `ZDOTDIR` to set without exporting (may be empty).
const SHELL_SCRIPT: &str = r#"[[ -n $1 ]] && source "$1"
[[ -n $3 ]] && ZDOTDIR=$3
source "$2"
print -r -- SHELL_KEPT
"#;

const UNTOUCHED: &str = "ZDOTDIR=unset GHOSTTY_ZSH_ZDOTDIR=unset KITTY_ORIG_ZDOTDIR=unset\n";

fn write_exe(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Writes the terminal's integration directory and returns the file a
/// terminal-injected shell would already have loaded.
fn write_integration(dir: &Path, term: Terminal, kind: Integration) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let hook = term.hook();
    let mut body = format!("{hook}() {{ :; }}\n");
    if !matches!(kind, Integration::NotPending) {
        body.push_str(&format!("precmd_functions+=({hook})\n"));
    }
    let file = dir.join("integration.zsh");
    std::fs::write(&file, body).unwrap();
    if !matches!(kind, Integration::NoZshenv) {
        std::fs::write(
            dir.join(".zshenv"),
            ZSHENV.replace("RESTORE_VAR", term.restore_var()),
        )
        .unwrap();
    }
    file
}

/// Sources the real `init.zsh` in a shell the terminal `term` started, with
/// `proxy` as the `ghost-complete` on `PATH`. Returns the proxy's output, the
/// integration directory as `:A` resolves it, and `$HOME`.
fn source_init_zsh(
    term: Terminal,
    branch: Branch,
    integration: Option<Integration>,
    zdotdir: UserZdotdir,
    extra_env: &[(&str, &str)],
    proxy: &str,
    user_zshenv: &str,
) -> (String, String, String) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_str().unwrap().to_owned();
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    write_exe(
        &bin.join("ghost-complete"),
        &proxy.replace("HOOK", term.hook()),
    );
    std::fs::write(tmp.path().join(".zshenv"), user_zshenv).unwrap();
    let script = tmp.path().join("shell.zsh");
    std::fs::write(&script, SHELL_SCRIPT).unwrap();
    let integration_dir = tmp.path().join("integration");
    let integration_file = match integration {
        Some(kind) => write_integration(&integration_dir, term, kind).into_os_string(),
        None => "".into(),
    };
    let init_zsh = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../shell/init.zsh");
    let not_exported = match zdotdir {
        UserZdotdir::NotExported(dir) => dir.replace("$HOME", &home),
        _ => String::new(),
    };

    let mut cmd = Command::new("zsh");
    cmd.arg("-f")
        .arg(&script)
        .arg(&integration_file)
        .arg(&init_zsh)
        .arg(&not_exported)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .stdin(Stdio::null());
    match term {
        Terminal::Ghostty => cmd.env("TERM_PROGRAM", "ghostty"),
        Terminal::Kitty => cmd.env("KITTY_WINDOW_ID", "1"),
    };
    if let Branch::Tmux = branch {
        cmd.env("TMUX", "/tmp/tmux-test/default,1,0")
            .env("TMUX_PANE", "%1")
            .env("GHOSTTY_RESOURCES_DIR", &home);
    }
    if let UserZdotdir::Exported(dir) = zdotdir {
        cmd.env("ZDOTDIR", dir);
    }
    cmd.envs(extra_env.iter().copied());
    let output = cmd.output().expect("run zsh");
    assert!(
        output.status.success(),
        "zsh failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let resolved = std::fs::canonicalize(&integration_dir).unwrap_or(integration_dir);
    (
        String::from_utf8(output.stdout).unwrap(),
        resolved.to_str().unwrap().to_owned(),
        home,
    )
}

/// The common case: print what the proxy received.
fn proxy_env(
    term: Terminal,
    branch: Branch,
    integration: Option<Integration>,
    zdotdir: UserZdotdir,
    extra_env: &[(&str, &str)],
) -> (String, String) {
    let (out, dir, _) = source_init_zsh(
        term,
        branch,
        integration,
        zdotdir,
        extra_env,
        PRINT_ENV_PROXY,
        "",
    );
    (out, dir)
}

/// What the proxy should see: `ZDOTDIR`, `term`'s restore variable set to
/// `restore`, and the other terminal's variable unset.
fn expected(term: Terminal, zdotdir: &str, restore: &str) -> String {
    let (ghostty, kitty) = match term {
        Terminal::Ghostty => (restore, "unset"),
        Terminal::Kitty => ("unset", restore),
    };
    format!("ZDOTDIR={zdotdir} GHOSTTY_ZSH_ZDOTDIR={ghostty} KITTY_ORIG_ZDOTDIR={kitty}\n")
}

#[test]
fn pending_integration_is_rearmed() {
    for term in Terminal::ALL {
        for branch in BRANCHES {
            let (out, dir) = proxy_env(
                term,
                branch,
                Some(Integration::Pending),
                UserZdotdir::Unset,
                &[],
            );
            assert_eq!(out, expected(term, &dir, "unset"), "{term:?} {branch:?}");
        }
    }
}

#[test]
fn exported_zdotdir_is_handed_to_the_terminal() {
    for term in Terminal::ALL {
        for branch in BRANCHES {
            let (out, dir) = proxy_env(
                term,
                branch,
                Some(Integration::Pending),
                UserZdotdir::Exported("/custom/zdot"),
                &[],
            );
            assert_eq!(
                out,
                expected(term, &dir, "/custom/zdot"),
                "{term:?} {branch:?}"
            );
        }
    }
}

/// `~/.zshenv` often sets `ZDOTDIR` without exporting it. A new shell does
/// not inherit that value and runs `~/.zshenv` again to set it; handing it to
/// the terminal would make the new shell skip `~/.zshenv`.
#[test]
fn unexported_zdotdir_is_not_handed_over() {
    for term in Terminal::ALL {
        for branch in BRANCHES {
            let (out, dir) = proxy_env(
                term,
                branch,
                Some(Integration::Pending),
                UserZdotdir::NotExported("/custom/zdot"),
                &[],
            );
            assert_eq!(out, expected(term, &dir, "unset"), "{term:?} {branch:?}");
        }
    }
}

/// A restore variable inherited from elsewhere would make the terminal's
/// `.zshenv` restore a `ZDOTDIR` this shell never had.
#[test]
fn stale_restore_variable_is_cleared() {
    for term in Terminal::ALL {
        for branch in BRANCHES {
            let (out, dir) = proxy_env(
                term,
                branch,
                Some(Integration::Pending),
                UserZdotdir::Unset,
                &[(term.restore_var(), "/stale")],
            );
            assert_eq!(out, expected(term, &dir, "unset"), "{term:?} {branch:?}");
        }
    }
}

/// Without the terminal's `.zshenv`, nothing would restore the user's
/// `ZDOTDIR` and zsh would skip the user's startup files.
#[test]
fn integration_without_zshenv_is_left_alone() {
    for term in Terminal::ALL {
        for branch in BRANCHES {
            let (out, _) = proxy_env(
                term,
                branch,
                Some(Integration::NoZshenv),
                UserZdotdir::Exported("/custom/zdot"),
                &[],
            );
            assert_eq!(
                out, "ZDOTDIR=/custom/zdot GHOSTTY_ZSH_ZDOTDIR=unset KITTY_ORIG_ZDOTDIR=unset\n",
                "{term:?} {branch:?}"
            );
        }
    }
}

/// The hook already ran, so the terminal didn't inject this shell (or the
/// user loaded the integration by hand, after a prompt).
#[test]
fn integration_that_already_ran_is_left_alone() {
    for term in Terminal::ALL {
        for branch in BRANCHES {
            let (out, _) = proxy_env(
                term,
                branch,
                Some(Integration::NotPending),
                UserZdotdir::Unset,
                &[],
            );
            assert_eq!(out, UNTOUCHED, "{term:?} {branch:?}");
        }
    }
}

#[test]
fn shell_without_terminal_integration_is_left_alone() {
    for term in Terminal::ALL {
        for branch in BRANCHES {
            let (out, _) = proxy_env(term, branch, None, UserZdotdir::Unset, &[]);
            assert_eq!(out, UNTOUCHED, "{term:?} {branch:?}");
        }
    }
}

/// The whole chain: the shell the proxy starts runs the terminal's `.zshenv`,
/// runs the user's `~/.zshenv`, ends up with the user's own `ZDOTDIR`, and
/// has the terminal's hook pending again.
#[test]
fn shell_behind_the_proxy_loads_the_integration() {
    for term in Terminal::ALL {
        // ~/.zshenv sets nothing.
        let (out, _, _) = source_init_zsh(
            term,
            Branch::Direct,
            Some(Integration::Pending),
            UserZdotdir::Unset,
            &[],
            SPAWN_SHELL_PROXY,
            "USER_ZSHENV_RAN=yes\n",
        );
        assert_eq!(out, "pending=yes zshenv=yes ZDOTDIR=unset\n", "{term:?}");

        // ~/.zshenv sets ZDOTDIR without exporting it.
        let (out, _, home) = source_init_zsh(
            term,
            Branch::Direct,
            Some(Integration::Pending),
            UserZdotdir::NotExported("$HOME/zdot"),
            &[],
            SPAWN_SHELL_PROXY,
            "USER_ZSHENV_RAN=yes\nZDOTDIR=$HOME/zdot\n",
        );
        assert_eq!(
            out,
            format!("pending=yes zshenv=yes ZDOTDIR={home}/zdot\n"),
            "{term:?}"
        );
    }
}
