use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use gc_parser::TerminalParser;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;

use gc_config::GhostConfig;

use crate::handler::InputHandler;
use crate::input::parse_keys;
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
/// stdin/stdout and the PTY until the shell exits. Keystrokes are routed
/// through the InputHandler for suggestion popup interception.
///
/// Returns the shell's exit code.
pub async fn run_proxy(shell: &str, args: &[String], config: &GhostConfig) -> Result<i32> {
    let SpawnedShell { master, mut child } = spawn_shell(shell, args)?;

    let mut reader = master
        .try_clone_reader()
        .context("failed to clone PTY reader")?;
    let writer = master.take_writer().context("failed to take PTY writer")?;

    // Enter raw mode with a drop guard so it's ALWAYS restored
    let _raw_guard = RawModeGuard::enable()?;

    // Initialize terminal parser with current screen dimensions
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let parser = Arc::new(Mutex::new(TerminalParser::new(rows, cols)));

    // Resolve spec directories from config
    let spec_dirs = resolve_spec_dirs(&config.paths.spec_dirs);

    // Initialize suggestion handler with config
    let handler = Arc::new(Mutex::new({
        let h = InputHandler::new(&spec_dirs[0]).unwrap_or_else(|e| {
            tracing::warn!(
                "failed to init suggestion engine: {}, suggestions disabled",
                e
            );
            InputHandler::new(std::path::Path::new(".")).expect("fallback handler")
        });
        h.with_popup_config(
            config.popup.max_visible,
            config.popup.min_width,
            config.popup.max_width,
        )
        .with_trigger_chars(&config.trigger.auto_chars)
        .with_suggest_config(
            config.suggest.max_results,
            config.suggest.max_history_entries,
            config.suggest.providers.commands,
            config.suggest.providers.history,
            config.suggest.providers.filesystem,
            config.suggest.providers.specs,
            config.suggest.providers.git,
        )
    }));

    // Channel to signal that one of the I/O tasks has finished
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

    // Task A: stdin → PTY (user keystrokes to shell, with popup interception)
    let stdin_shutdown = shutdown_tx.clone();
    let mut pty_writer = writer;
    let parser_for_stdin = Arc::clone(&parser);
    let handler_for_stdin = Arc::clone(&handler);
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

            let keys = parse_keys(&buf[..n]);
            for key in &keys {
                // Handler writes popup rendering into a buffer instead of
                // locking stdout for the entire loop (which would deadlock
                // with Task B's stdout writes).
                let mut render_buf = Vec::new();
                let forward = {
                    let mut h = handler_for_stdin.lock().unwrap();
                    h.process_key(key, &parser_for_stdin, &mut render_buf)
                };
                // Briefly lock stdout to flush any popup rendering
                if !render_buf.is_empty() {
                    let mut stdout = std::io::stdout().lock();
                    let _ = stdout.write_all(&render_buf);
                    let _ = stdout.flush();
                }
                if !forward.is_empty() {
                    if pty_writer.write_all(&forward).is_err() {
                        return;
                    }
                    if pty_writer.flush().is_err() {
                        return;
                    }
                }
            }
        }
        let _ = stdin_shutdown.try_send(());
    });

    // Task B: PTY → stdout (shell output to terminal)
    let pty_shutdown = shutdown_tx.clone();
    let parser_for_stdout = Arc::clone(&parser);
    let handler_for_stdout = Arc::clone(&handler);
    let stdout_handle = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 8192];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break, // PTY closed
                Ok(n) => n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };

            // Feed bytes through the VT parser to track terminal state
            {
                let mut p = parser_for_stdout.lock().unwrap();
                p.process_bytes(&buf[..n]);
            }

            // Briefly lock stdout for each write — do NOT hold the lock
            // across the entire loop or it deadlocks with Task A.
            {
                let mut stdout = std::io::stdout().lock();
                if stdout.write_all(&buf[..n]).is_err() {
                    break;
                }
                if stdout.flush().is_err() {
                    break;
                }
            }

            // Check if shell reported a buffer update via OSC 7770.
            // Trigger suggestions here (Task B) instead of Task A to ensure
            // we have the shell's updated buffer, fixing the stale-buffer bug.
            let buffer_dirty = {
                let mut p = parser_for_stdout.lock().unwrap();
                p.state_mut().take_buffer_dirty()
            };

            if buffer_dirty {
                let mut render_buf = Vec::new();
                {
                    let mut h = handler_for_stdout.lock().unwrap();
                    if h.has_pending_trigger() {
                        h.clear_trigger_request();
                        h.trigger(&parser_for_stdout, &mut render_buf);
                    }
                }
                if !render_buf.is_empty() {
                    let mut stdout = std::io::stdout().lock();
                    let _ = stdout.write_all(&render_buf);
                    let _ = stdout.flush();
                }
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
                        // Update parser's screen dimensions
                        {
                            let mut p = parser.lock().unwrap();
                            p.state_mut().update_dimensions(size.rows, size.cols);
                        }
                        // Re-render popup if visible
                        {
                            let mut stdout = std::io::stdout().lock();
                            let mut h = handler.lock().unwrap();
                            h.handle_resize(&parser, &mut stdout);
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

/// Resolve spec directories from config, with tilde expansion.
/// If config provides directories, use those. Otherwise fall back to auto-detection.
fn resolve_spec_dirs(configured: &[String]) -> Vec<PathBuf> {
    if !configured.is_empty() {
        return configured
            .iter()
            .map(|s| expand_tilde(s))
            .filter(|p| p.is_dir())
            .collect();
    }

    // Auto-detect: check next to binary, then cwd
    let mut dirs = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let spec_dir = exe_dir.join("specs");
            if spec_dir.is_dir() {
                dirs.push(spec_dir);
            }
        }
    }

    // Fall back to specs/ in the current directory (development)
    let cwd_specs = PathBuf::from("specs");
    if cwd_specs.is_dir() {
        dirs.push(cwd_specs);
    }

    if dirs.is_empty() {
        dirs.push(PathBuf::from("specs"));
    }

    dirs
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}
