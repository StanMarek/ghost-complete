use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::BorrowedFd;
use std::os::unix::process::CommandExt;
use std::path::Path;

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtyPair};

use crate::resize::get_terminal_size;

/// Set on a shell that runs in place of a proxy that failed to start, to the
/// pid the shell inherited from that proxy. `init.zsh` leaves the proxy alone
/// when that pid is the shell itself or one of its ancestors (a `$SHELL`
/// wrapper that forks zsh), then unsets the marker, so subshells and new tabs
/// still launch the proxy.
pub const FALLBACK_PID_ENV: &str = "GHOST_COMPLETE_FALLBACK_PID";

pub struct SpawnedShell {
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
}

/// Open a PTY sized like our terminal and start `shell` on it.
///
/// Everything that can fail runs before the shell starts, so an `Err` never
/// leaves a shell behind.
pub fn spawn_shell(shell: &OsStr, args: &[OsString]) -> Result<SpawnedShell> {
    let size = get_terminal_size().context("failed to query terminal size")?;

    let pty_system = native_pty_system();
    let PtyPair { master, slave } = pty_system
        .openpty(size)
        .context("failed to open PTY pair")?;
    let reader = master
        .try_clone_reader()
        .context("failed to clone PTY reader")?;
    let writer = pty_writer(master.as_ref())?;

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

    Ok(SpawnedShell {
        master,
        child,
        reader,
        writer,
    })
}

/// Replace this process with `shell`, run without the proxy. Returns only if
/// that fails.
///
/// Refuses when this process, or its parent, already did it once: the shell
/// it ran then started the proxy again (directly, or from a zsh it started),
/// which means the `init.zsh` involved predates [`FALLBACK_PID_ENV`], and
/// another fallback would loop forever.
pub fn exec_plain_shell(shell: &OsStr, args: &[OsString]) -> anyhow::Error {
    let shell_display = Path::new(shell).display();
    let pid = std::process::id().to_string();
    // SAFETY: getppid(2) has no preconditions and cannot fail.
    let parent = unsafe { libc::getppid() }.to_string();
    if std::env::var_os(FALLBACK_PID_ENV)
        .is_some_and(|marker| marker == pid.as_str() || marker == parent.as_str())
    {
        return anyhow::anyhow!(
            "not starting {shell_display} again: it already ran in place of the proxy \
             and started the proxy back up, so the init.zsh it sourced is out of date \
             (run `ghost-complete install`)"
        );
    }

    let mut cmd = std::process::Command::new(shell);
    cmd.args(args)
        // No proxy is listening. With this set, ghost-complete.zsh would send
        // its private OSC frames straight to the terminal on every keystroke.
        .env_remove("GHOST_COMPLETE_ACTIVE")
        .env(FALLBACK_PID_ENV, &pid);
    // Same reason as in `spawn_shell`: the IMDS override is ours, not the user's.
    if gc_suggest::aws::imds_disabled_was_injected() {
        cmd.env_remove(gc_suggest::aws::IMDS_DISABLED_ENV);
    }
    // `exec` is execvp(3): `shell` is never interpreted by a shell.
    let err = cmd.exec();
    anyhow::Error::new(err).context(format!("failed to exec {shell_display}"))
}

/// Open a writer onto the PTY master.
///
/// Deliberately not `MasterPty::take_writer`: portable-pty's writer types
/// `\n` + VEOF into the PTY when dropped. Task A drops its writer when our
/// terminal goes away, so whatever sat on the shell's command line would run
/// as the window closed. A plain dup of the master fd has no drop side effect.
pub fn pty_writer(master: &dyn MasterPty) -> Result<Box<dyn Write + Send>> {
    let fd = master
        .as_raw_fd()
        .context("PTY master has no file descriptor")?;
    // SAFETY: `fd` is owned by `master`, which outlives this borrow;
    // `try_clone_to_owned` dups it, so the writer owns an fd of its own.
    let fd = unsafe { BorrowedFd::borrow_raw(fd) }
        .try_clone_to_owned()
        .context("failed to duplicate PTY master fd")?;
    Ok(Box::new(File::from(fd)))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    use portable_pty::{native_pty_system, CommandBuilder, PtySize};

    use super::pty_writer;

    /// Wait up to 200 ms for `fd` to turn readable.
    fn readable(fd: std::os::fd::RawFd) -> bool {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pfd` is a valid pollfd for the duration of the call.
        let ready = unsafe { libc::poll(&mut pfd, 1, 200) };
        ready > 0
    }

    /// Task A drops its PTY writer when the proxy's terminal goes away. If
    /// that drop types anything into the shell, a half-typed command line is
    /// submitted on window close. The slave is in canonical mode, so it only
    /// turns readable once a line ends (`\n`) or VEOF arrives.
    #[test]
    fn dropping_pty_writer_types_nothing_into_the_shell() {
        let pair = native_pty_system()
            .openpty(PtySize::default())
            .expect("openpty");
        let tty = pair.master.tty_name().expect("slave tty name");
        let mut slave = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOCTTY)
            .open(&tty)
            .expect("open slave tty");

        let mut writer = pty_writer(pair.master.as_ref()).expect("pty_writer");
        writer.write_all(b"ok\n").expect("write to PTY");
        assert!(
            readable(slave.as_raw_fd()),
            "written line never reached the shell"
        );
        let mut line = [0u8; 16];
        let n = slave.read(&mut line).expect("read slave");
        assert_eq!(&line[..n], b"ok\n");

        drop(writer);

        assert!(
            !readable(slave.as_raw_fd()),
            "dropping the PTY writer typed more input into the shell \
             (portable-pty's writer sends `\\n` + VEOF on drop)"
        );
    }

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
