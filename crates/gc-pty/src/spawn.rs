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

    // Inherit the current environment
    for (key, value) in std::env::vars() {
        cmd.env(key, value);
    }

    let child = slave
        .spawn_command(cmd)
        .context("failed to spawn shell process")?;

    // Drop slave — parent must not hold the slave FD
    drop(slave);

    Ok(SpawnedShell { master, child })
}
