use std::ffi::{OsStr, OsString};

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtyPair};

use crate::resize::get_terminal_size;

pub struct SpawnedShell {
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
}

pub fn spawn_shell(shell: &OsStr, args: &[OsString]) -> Result<SpawnedShell> {
    let size = get_terminal_size().context("failed to query terminal size")?;

    let pty_system = native_pty_system();
    let PtyPair { master, slave } = pty_system
        .openpty(size)
        .context("failed to open PTY pair")?;

    let mut cmd = CommandBuilder::new(shell);
    cmd.args(args);
    cmd.cwd(std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/")));

    // Inherit the current environment. `CommandBuilder::new` already
    // pre-populates the env from `std::env::vars_os()` at construction
    // time, so this loop is redundant for the inheritance path itself
    // — but we keep it as the canonical handoff point and so that
    // explicit overrides below (`GHOST_COMPLETE_ACTIVE`, `GHOST_COMPLETE_PANE`)
    // stay in one block.
    for (key, value) in std::env::vars() {
        cmd.env(key, value);
    }
    // Strip AWS_EC2_METADATA_DISABLED if WE injected it at startup
    // (set_imds_disabled_env in `fn main`). The base env that
    // `CommandBuilder::new` snapshots already contains the var by then,
    // so `env_remove` is required — skipping it in the loop above is
    // not sufficient. Without this, the proxy silently overrides an
    // AWS SDK knob in every command the user runs inside the shell,
    // breaking the "PTY proxy is invisible" contract.
    if gc_suggest::aws::imds_disabled_was_injected() {
        cmd.env_remove(gc_suggest::aws::IMDS_DISABLED_ENV);
    }
    // Belt-and-suspenders recursion guard. init.zsh checks this in the
    // non-tmux path; setting it here covers manual `ghost-complete` invocations
    // that bypass init.zsh entirely.
    cmd.env("GHOST_COMPLETE_ACTIVE", "1");

    // Pane-local recursion guard for tmux. init.zsh compares this against the
    // live $TMUX_PANE — matches inside the same pane (blocking subshells),
    // mismatches in new panes (allowing a fresh proxy).
    if std::env::var("TMUX").is_ok() {
        match std::env::var("TMUX_PANE") {
            Ok(pane) => {
                cmd.env("GHOST_COMPLETE_PANE", pane);
            }
            Err(_) => tracing::warn!(
                "TMUX is set but TMUX_PANE is not — subshell recursion guard degraded"
            ),
        }
    }

    let child = slave
        .spawn_command(cmd)
        .context("failed to spawn shell process")?;

    // Drop slave — parent must not hold the slave FD
    drop(slave);

    Ok(SpawnedShell { master, child })
}

#[cfg(test)]
mod tests {
    use portable_pty::CommandBuilder;

    /// Reproduces the bug Codex flagged: `CommandBuilder::new`
    /// pre-snapshots the parent process env at construction time, so
    /// merely skipping a key inside the explicit-`env` inheritance
    /// loop is NOT sufficient to strip it from the child. An explicit
    /// `env_remove` after construction is the only correct way.
    ///
    /// We use `PATH` because it is universally set by the test runner
    /// and observing it requires zero process-env mutation — critical
    /// under `cargo test`'s parallel harness, where any `set_var` in
    /// one test races with every other test's env reads.
    #[test]
    fn command_builder_base_env_must_be_explicitly_stripped() {
        // Precondition for the test itself, not the code under test.
        assert!(
            std::env::var_os("PATH").is_some(),
            "test runner must export PATH"
        );

        let mut cmd = CommandBuilder::new("/bin/true");
        let before = cmd.get_env("PATH").map(|v| v.to_owned());
        cmd.env_remove("PATH");
        let after = cmd.get_env("PATH").map(|v| v.to_owned());

        assert!(
            before.is_some(),
            "CommandBuilder::new must pre-snapshot parent env from std::env"
        );
        assert!(
            after.is_none(),
            "env_remove must clear the pre-snapshotted entry — the production \
             fix in spawn_shell relies on this contract"
        );
    }
}
