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

    /// Reproduces the bug Codex flagged: `CommandBuilder::new` already
    /// snapshots `AWS_EC2_METADATA_DISABLED` into its base env when the
    /// var is set in the parent process, so merely skipping the key
    /// inside the explicit-`env` loop is NOT sufficient. We need an
    /// explicit `env_remove` for the strip to take effect.
    #[test]
    fn command_builder_base_env_must_be_explicitly_stripped() {
        // Snapshot any prior value so we don't bleed test state.
        let prior = std::env::var_os("GC_TEST_BASE_ENV_PROBE");

        // SAFETY: this test process is fully single-threaded by the
        // time Rust's test harness invokes the test body for a leaf
        // function with no `#[tokio::test]` / no spawned threads. We
        // restore the prior value before returning.
        unsafe {
            std::env::set_var("GC_TEST_BASE_ENV_PROBE", "set-by-parent");
        }

        let mut cmd = CommandBuilder::new("/bin/true");
        // Without env_remove, the base env still carries the value.
        let before = cmd.get_env("GC_TEST_BASE_ENV_PROBE").map(|v| v.to_owned());
        cmd.env_remove("GC_TEST_BASE_ENV_PROBE");
        let after = cmd.get_env("GC_TEST_BASE_ENV_PROBE").map(|v| v.to_owned());

        // Restore parent env before any assertion that could fail.
        // SAFETY: same justification as above.
        unsafe {
            if let Some(v) = prior {
                std::env::set_var("GC_TEST_BASE_ENV_PROBE", v);
            } else {
                std::env::remove_var("GC_TEST_BASE_ENV_PROBE");
            }
        }

        assert!(
            before.is_some(),
            "CommandBuilder::new must pre-snapshot parent env"
        );
        assert!(
            after.is_none(),
            "env_remove must clear the pre-snapshotted entry"
        );
    }
}
