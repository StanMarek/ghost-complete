use std::io::{Read, Write};

use anyhow::{Context, Result};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;

use crate::resize::{get_terminal_size, resize_pty};
use crate::spawn::{spawn_shell, SpawnedShell};

/// Drop guard that ensures raw mode is always restored, even on panic.
struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> Result<Self> {
        crossterm::terminal::enable_raw_mode().context("failed to enable raw mode")?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Run the PTY proxy event loop. This is the main entry point for the proxy.
///
/// Spawns the given shell, enters raw mode, and forwards all I/O between
/// stdin/stdout and the PTY until the shell exits.
///
/// Returns the shell's exit code.
pub async fn run_proxy(shell: &str, args: &[String]) -> Result<i32> {
    let SpawnedShell { master, mut child } = spawn_shell(shell, args)?;

    let mut reader = master
        .try_clone_reader()
        .context("failed to clone PTY reader")?;
    let writer = master.take_writer().context("failed to take PTY writer")?;

    // Enter raw mode with a drop guard so it's ALWAYS restored
    let _raw_guard = RawModeGuard::enable()?;

    // Channel to signal that one of the I/O tasks has finished
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

    // Task A: stdin → PTY (user keystrokes to shell)
    let stdin_shutdown = shutdown_tx.clone();
    let mut pty_writer = writer;
    let stdin_handle = tokio::task::spawn_blocking(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 256];
        loop {
            let n = match stdin.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            if pty_writer.write_all(&buf[..n]).is_err() {
                break;
            }
            if pty_writer.flush().is_err() {
                break;
            }
        }
        let _ = stdin_shutdown.try_send(());
    });

    // Task B: PTY → stdout (shell output to terminal)
    let pty_shutdown = shutdown_tx.clone();
    let stdout_handle = tokio::task::spawn_blocking(move || {
        let mut stdout = std::io::stdout().lock();
        let mut buf = [0u8; 8192];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break, // PTY closed
                Ok(n) => n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            if stdout.write_all(&buf[..n]).is_err() {
                break;
            }
            if stdout.flush().is_err() {
                break;
            }
        }
        let _ = pty_shutdown.try_send(());
    });

    // Drop the sender we cloned from — we only need the ones in the tasks
    drop(shutdown_tx);

    // Task C: Signal handling
    let mut sigwinch =
        signal(SignalKind::window_change()).context("failed to register SIGWINCH handler")?;

    // Wait for either an I/O task to finish or a signal
    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                tracing::debug!("I/O task finished, shutting down");
                break;
            }
            _ = sigwinch.recv() => {
                match get_terminal_size() {
                    Ok(size) => {
                        if let Err(e) = resize_pty(master.as_ref(), size) {
                            tracing::warn!("failed to resize PTY: {}", e);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("failed to get terminal size for resize: {}", e);
                    }
                }
            }
        }
    }

    // Clean up: abort I/O tasks (they'll be blocked on reads)
    stdin_handle.abort();
    stdout_handle.abort();

    // _raw_guard drops here, restoring terminal state

    // Wait for child and get exit status
    let status = child.wait().context("failed to wait for shell process")?;
    let exit_code = status.exit_code().try_into().unwrap_or(1);

    Ok(exit_code)
}
