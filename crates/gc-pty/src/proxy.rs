use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use gc_parser::TerminalParser;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{mpsc, Notify};

use gc_config::GhostConfig;

use gc_overlay::{parse_style, PopupTheme};

use crate::handler::{InputHandler, Keybindings};
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
    // Log tmux detection for debugging
    if std::env::var("TMUX").is_ok() {
        tracing::info!("tmux session detected — running inside tmux pane");
        if let Ok(output) = std::process::Command::new("tmux").arg("-V").output() {
            let version = String::from_utf8_lossy(&output.stdout);
            tracing::info!("tmux version: {}", version.trim());
        }
    }

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

    // Resolve keybindings from config (fail fast on invalid key names)
    let keybindings = Keybindings::from_config(&config.keybindings)?;

    // Resolve theme from config (fail fast on invalid preset or style strings)
    let resolved_theme = config.theme.resolve().context("invalid theme preset")?;
    let theme = PopupTheme {
        selected_on: parse_style(&resolved_theme.selected)
            .context("invalid theme.selected style")?,
        description_on: parse_style(&resolved_theme.description)
            .context("invalid theme.description style")?,
        match_highlight_on: parse_style(&resolved_theme.match_highlight)
            .context("invalid theme.match_highlight style")?,
        item_text_on: parse_style(&resolved_theme.item_text)
            .context("invalid theme.item_text style")?,
        scrollbar_on: parse_style(&resolved_theme.scrollbar)
            .context("invalid theme.scrollbar style")?,
    };

    // Initialize suggestion handler with config
    let handler = Arc::new(Mutex::new({
        let h = InputHandler::new(&spec_dirs[0]).unwrap_or_else(|e| {
            tracing::warn!(
                "failed to init suggestion engine: {}, suggestions disabled",
                e
            );
            InputHandler::new(std::path::Path::new(".")).expect("fallback handler")
        });
        h.with_keybindings(keybindings)
            .with_theme(theme)
            .with_popup_config(
                config.popup.max_visible,
                config.popup.min_width,
                config.popup.max_width,
            )
            .with_trigger_chars(&config.trigger.auto_chars)
            .with_generator_timeout(config.suggest.generator_timeout_ms)
            .with_suggest_config(
                config.suggest.max_results,
                config.suggest.max_history_entries,
                config.suggest.providers.commands,
                config.suggest.max_history_results,
                config.suggest.providers.filesystem,
                config.suggest.providers.specs,
                config.suggest.providers.git,
            )
    }));

    // Debounce task: fires suggestions after a typing pause
    let debounce_notify = Arc::new(Notify::new());
    let delay_ms = config.trigger.delay_ms;

    let debounce_handle = if delay_ms > 0 {
        let notify = Arc::clone(&debounce_notify);
        let handler_d = Arc::clone(&handler);
        let parser_d = Arc::clone(&parser);
        Some(tokio::spawn(async move {
            debounce_loop(notify, handler_d, parser_d, delay_ms).await;
        }))
    } else {
        None
    };

    // Task E: dynamic merge loop — renders script generator results when shell is idle.
    let dynamic_notify = {
        let h = handler.lock().unwrap();
        h.dynamic_notify()
    };
    let handler_for_merge = Arc::clone(&handler);
    let parser_for_merge = Arc::clone(&parser);
    let merge_handle = tokio::spawn(async move {
        dynamic_merge_loop(dynamic_notify, handler_for_merge, parser_for_merge).await;
    });

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
                // Intercept CPR (Cursor Position Report) responses —
                // update parser with real cursor position, don't forward
                // to the shell PTY.
                if let crate::input::KeyEvent::CursorPositionReport(row, col) = key {
                    let mut p = parser_for_stdin.lock().unwrap();
                    tracing::debug!(row, col, "CPR response — syncing cursor position");
                    p.state_mut().set_cursor_from_report(*row, *col);
                    continue;
                }

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
    let debounce_notify_b = Arc::clone(&debounce_notify);
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
            let needs_cpr = {
                let mut p = parser_for_stdout.lock().unwrap();
                p.process_bytes(&buf[..n]);
                p.state_mut().take_cursor_sync_requested()
            };

            // Briefly lock stdout for each write — do NOT hold the lock
            // across the entire loop or it deadlocks with Task A.
            {
                let mut stdout = std::io::stdout().lock();
                if stdout.write_all(&buf[..n]).is_err() {
                    break;
                }
                // Send CPR request (CSI 6n) to the REAL terminal so it
                // reports its actual cursor position. The response
                // (CSI row;col R) arrives on stdin and is intercepted by
                // Task A to sync our VT parser's cursor tracking.
                if needs_cpr {
                    tracing::debug!("sending CPR request (CSI 6n)");
                    let _ = stdout.write_all(b"\x1b[6n");
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
                    } else if delay_ms > 0 {
                        debounce_notify_b.notify_one();
                    }
                }
                if !render_buf.is_empty() {
                    let mut stdout = std::io::stdout().lock();
                    let _ = stdout.write_all(&render_buf);
                    let _ = stdout.flush();
                }
            }

            // CD chaining: auto-trigger suggestions when CWD changes (OSC 7).
            // No has_pending_trigger() gate — CWD change is unconditional.
            let cwd_dirty = {
                let mut p = parser_for_stdout.lock().unwrap();
                p.state_mut().take_cwd_dirty()
            };

            if cwd_dirty {
                let mut render_buf = Vec::new();
                {
                    let mut h = handler_for_stdout.lock().unwrap();
                    h.trigger(&parser_for_stdout, &mut render_buf);
                }
                if !render_buf.is_empty() {
                    let mut stdout = std::io::stdout().lock();
                    let _ = stdout.write_all(&render_buf);
                    let _ = stdout.flush();
                }
            }

            // Poll for dynamic (script generator) results — non-blocking.
            {
                let mut render_buf = Vec::new();
                {
                    let mut h = handler_for_stdout.lock().unwrap();
                    h.try_merge_dynamic(&parser_for_stdout, &mut render_buf);
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
    merge_handle.abort();
    if let Some(h) = debounce_handle {
        h.abort();
    }

    // _raw_guard drops here, restoring terminal state

    // Wait for child and get exit status
    let status = child.wait().context("failed to wait for shell process")?;
    let exit_code = status.exit_code().try_into().unwrap_or(1);

    Ok(exit_code)
}

/// Debounce loop: waits for buffer-change notifications, resets a timer on each
/// new notification, and fires suggestions once the timer expires (typing pause).
async fn debounce_loop(
    notify: Arc<Notify>,
    handler: Arc<Mutex<InputHandler>>,
    parser: Arc<Mutex<TerminalParser>>,
    delay_ms: u64,
) {
    let delay = std::time::Duration::from_millis(delay_ms);
    loop {
        // Wait for first buffer change notification
        notify.notified().await;

        // Debounce: reset timer on every new notification
        loop {
            tokio::select! {
                _ = notify.notified() => { continue; }
                _ = tokio::time::sleep(delay) => { break; }
            }
        }

        // Timer expired — fire trigger
        let mut render_buf = Vec::new();
        {
            let mut h = handler.lock().unwrap();
            h.trigger(&parser, &mut render_buf);
        }
        if !render_buf.is_empty() {
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(&render_buf);
            let _ = stdout.flush();
        }
    }
}

/// Dynamic merge loop: awaits notification from script generator tasks and
/// merges results into the popup. This ensures dynamic results render even
/// when the shell is idle (no PTY output flowing through Task B).
async fn dynamic_merge_loop(
    notify: Arc<Notify>,
    handler: Arc<Mutex<InputHandler>>,
    parser: Arc<Mutex<TerminalParser>>,
) {
    loop {
        notify.notified().await;
        let mut render_buf = Vec::new();
        {
            let mut h = handler.lock().unwrap();
            h.try_merge_dynamic(&parser, &mut render_buf);
        }
        if !render_buf.is_empty() {
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(&render_buf);
            let _ = stdout.flush();
        }
    }
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

    // Auto-detect: check config dir, next to binary, then cwd
    let mut dirs = Vec::new();

    // Config directory (installed by `ghost-complete install`)
    if let Some(config_dir) = gc_config::config_dir() {
        let spec_dir = config_dir.join("specs");
        if spec_dir.is_dir() {
            dirs.push(spec_dir);
        }
    }

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
