use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtyPair};

use crate::resize::get_terminal_size;

pub struct SpawnedShell {
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
}

pub fn spawn_shell(shell: &str, args: &[String]) -> Result<SpawnedShell> {
    let size = get_terminal_size().context("failed to query terminal size")?;

    let pty_system = native_pty_system();
    let PtyPair { master, slave } = pty_system
        .openpty(size)
        .context("failed to open PTY pair")?;

    let mut cmd = CommandBuilder::new(shell);
    cmd.args(args);
    cmd.cwd(std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/")));

    // Inherit the current environment
    for (key, value) in std::env::vars() {
        cmd.env(key, value);
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
