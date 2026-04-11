use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use gc_parser::TerminalParser;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{mpsc, Notify};

use gc_config::GhostConfig;

use gc_overlay::{parse_style, PopupTheme};
use gc_suggest::spec_dirs::resolve_spec_dirs;

use crate::config_watch::spawn_config_watcher;
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
    // Detect terminal capabilities
    let terminal_profile = gc_terminal::TerminalProfile::detect();
    if matches!(
        terminal_profile.terminal(),
        gc_terminal::Terminal::Unknown(_)
    ) {
        tracing::warn!(
            terminal = %terminal_profile.terminal(),
            "running on unsupported terminal — cursor save/restore may not work correctly"
        );
        eprintln!(
            "ghost-complete: WARNING — {} is not a tested terminal. \
             Popup rendering may not work correctly.\n\
             Supported terminals: {}",
            terminal_profile.terminal(),
            gc_terminal::Terminal::supported_terminals().join(", ")
        );
    } else {
        tracing::info!(
            terminal = %terminal_profile.terminal(),
            render = %terminal_profile.render_strategy(),
            prompt = %terminal_profile.prompt_detection(),
            "terminal profile detected"
        );
    }

    // Gate unknown terminals behind experimental flag.
    // Known terminals (Ghostty, Kitty, WezTerm, Alacritty, Rio, iTerm2, Terminal.app)
    // work without any flag. Unknown terminals need multi_terminal = true.
    // Note: CommandExt::exec() is the Unix execvp() syscall — no shell
    // interpretation, no injection risk. `shell` comes from $SHELL or argv.
    if should_fallback_to_shell(
        terminal_profile.terminal(),
        config.experimental.multi_terminal,
    ) {
        tracing::warn!(
            terminal = %terminal_profile.terminal(),
            "unknown terminal requires [experimental] multi_terminal = true — falling back to plain shell"
        );
        eprintln!(
            "ghost-complete: {} is not a supported terminal.\n\
             To try anyway, add to ~/.config/ghost-complete/config.toml:\n\n  \
             [experimental]\n  \
             multi_terminal = true\n",
            terminal_profile.terminal()
        );
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(shell).args(args).exec();
        anyhow::bail!("failed to exec shell: {}", err);
    }

    // Log tmux detection and propagate recursion guard to future panes
    if std::env::var("TMUX").is_ok() {
        tracing::info!("tmux session detected — running inside tmux pane");
        if let Ok(output) = std::process::Command::new("tmux").arg("-V").output() {
            let version = String::from_utf8_lossy(&output.stdout);
            tracing::info!("tmux version: {}", version.trim());
        }
        // Propagate GHOST_COMPLETE_ACTIVE into the tmux session env so future
        // panes inherit it. init.zsh uses PPID + GHOST_COMPLETE_PANE for its
        // recursion check (not this variable), but session-level propagation
        // covers edge cases (respawn-pane, programmatic pane creation) and
        // lets script generators detect the proxy context.
        match std::process::Command::new("tmux")
            .args(["setenv", "GHOST_COMPLETE_ACTIVE", "1"])
            .output()
        {
            Ok(output) if !output.status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::warn!(
                    "tmux setenv failed (exit {}): {}",
                    output.status,
                    stderr.trim()
                );
            }
            Err(e) => tracing::warn!("failed to run tmux setenv: {}", e),
            _ => {}
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
        border_on: parse_style(&resolved_theme.border).context("invalid theme.border style")?,
        borders: config.popup.borders,
    };

    // Initialize suggestion handler with config
    let handler = Arc::new(Mutex::new({
        let h = match InputHandler::new(&spec_dirs, terminal_profile.clone()) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("failed to init suggestion engine: {}, trying fallback", e);
                InputHandler::new(&[std::path::PathBuf::from(".")], terminal_profile)
                    .context("fallback handler also failed — cannot start proxy")?
            }
        };
        h.with_keybindings(keybindings)
            .with_theme(theme)
            .with_popup_config(config.popup.max_visible)
            .with_trigger_chars(&config.trigger.auto_chars)
            .with_auto_trigger(config.trigger.auto_trigger)
            .with_suggest_config(
                config.suggest.max_results,
                config.suggest.providers.commands,
                config.suggest.max_history_results,
                config.suggest.providers.filesystem,
                config.suggest.providers.specs,
                config.suggest.providers.git,
                config.suggest.generator_timeout_ms,
            )
    }));

    // Config hot-reload: watch config.toml for changes
    let config_watcher_handle = if let Some(config_dir) = gc_config::config_dir() {
        let config_path = config_dir.join("config.toml");
        match spawn_config_watcher(config_path, Arc::clone(&handler)) {
            Ok(handle) => Some(handle),
            Err(e) => {
                tracing::warn!("failed to start config watcher: {e}");
                None
            }
        }
    } else {
        None
    };

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
        // This lock runs during startup before the handler `Arc` is shared
        // with any other task, so poison is extremely unlikely. We still
        // use the match-with-warn pattern for consistency with every other
        // lock site in this file.
        let h = match handler.lock() {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("handler mutex poisoned during setup: {e}");
                anyhow::bail!("handler mutex poisoned during setup — cannot start proxy");
            }
        };
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
        let mut buf = [0u8; 4096];
        'stdin: loop {
            let n = match stdin.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };

            let keys = parse_keys(&buf[..n]);
            for key in &keys {
                // CPR (Cursor Position Report) responses arrive here
                // from the real terminal. If we sent the request, consume
                // it for cursor sync. Otherwise forward it through the
                // PTY so programs like atuin/crossterm receive it.
                if let crate::input::KeyEvent::CursorPositionReport(row, col) = key {
                    let mut p = match parser_for_stdin.lock() {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!("parser mutex poisoned in stdin task: {e}");
                            break 'stdin;
                        }
                    };
                    if p.state_mut().claim_cpr_response() {
                        let state = p.state_mut();
                        if state.validate_cpr_coordinates(*row, *col) {
                            tracing::debug!(
                                row,
                                col,
                                "CPR response — syncing cursor position (ours)"
                            );
                            state.set_cursor_from_report(*row, *col);
                        } else {
                            let (screen_rows, screen_cols) = state.screen_dimensions();
                            tracing::warn!(
                                row,
                                col,
                                screen_rows,
                                screen_cols,
                                "CPR coordinates out of screen bounds — ignoring"
                            );
                            // Do NOT retry with a new CSI 6n: we can't tell if
                            // this was the terminal's real response (resize race)
                            // or an injected fake. Retrying would send a CSI 6n
                            // whose response may have no matching cpr_pending,
                            // leaking an unsolicited CPR into the PTY. Instead,
                            // accept the temporary cursor desync — the next
                            // prompt (OSC 133) triggers a fresh sync cycle.
                        }
                        continue;
                    }
                    tracing::debug!(row, col, "CPR response — forwarding to PTY (not ours)");
                    drop(p);
                    // Re-encode as CSI row;col R and forward to PTY
                    let cpr = format!("\x1b[{row};{col}R");
                    if pty_writer.write_all(cpr.as_bytes()).is_err() {
                        return;
                    }
                    if pty_writer.flush().is_err() {
                        return;
                    }
                    continue;
                }

                // Handler writes popup rendering into a buffer instead of
                // locking stdout for the entire loop (which would deadlock
                // with Task B's stdout writes).
                let mut render_buf = Vec::new();
                let forward = {
                    let mut h = match handler_for_stdin.lock() {
                        Ok(h) => h,
                        Err(e) => {
                            tracing::warn!("handler mutex poisoned in stdin task: {e}");
                            break 'stdin;
                        }
                    };
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
                let mut p = match parser_for_stdout.lock() {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!("parser mutex poisoned in stdout task: {e}");
                        break;
                    }
                };
                p.process_bytes(&buf[..n]);
                p.state_mut().take_cursor_sync_requested()
            };

            // Briefly lock stdout for each write — do NOT hold the lock
            // across the entire loop or it deadlocks with Task A.
            //
            // CPR accounting invariant: cpr_pending must be incremented
            // if and only if a corresponding CSI 6n was successfully
            // flushed to the terminal. Violating this in either direction
            // causes cursor-sync corruption — under-counting makes Task A
            // forward our own CPR response to the PTY, over-counting makes
            // Task A steal the next application CPR response.
            //
            // Lock ordering: acquire the parser mutex and drop it BEFORE
            // taking the stdout lock. A nested (stdout → parser) path is a
            // latent deadlock trap for any future code that legitimately
            // takes parser then stdout. cpr_pending is a counter, so
            // incrementing earlier (before the main buffer write) is
            // semantically equivalent — Task A always decrements on the
            // matching CPR response, and we roll back on write failure.
            let mut cpr_incremented = false;
            if needs_cpr {
                match parser_for_stdout.lock() {
                    Ok(mut p) => {
                        p.state_mut().increment_cpr_pending();
                        cpr_incremented = true;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "parser mutex poisoned before CPR increment: {e} \
                             — skipping CPR"
                        );
                    }
                }
            }
            // If we couldn't increment (poisoned mutex), don't emit the
            // CSI 6n — under-counting cpr_pending would make Task A
            // forward our response to the PTY.
            let send_cpr = needs_cpr && cpr_incremented;

            let write_result: std::io::Result<()> = {
                let mut stdout = std::io::stdout().lock();
                stdout.write_all(&buf[..n]).and_then(|()| {
                    if send_cpr {
                        tracing::debug!("sending CPR request (CSI 6n)");
                        stdout.write_all(b"\x1b[6n").and_then(|()| stdout.flush())
                    } else {
                        stdout.flush()
                    }
                })
            };

            if let Err(e) = write_result {
                // Rollback the CPR increment if the write/flush failed
                // partway through — the CSI 6n did not reach the terminal
                // (or at least can't be guaranteed to have reached it) so
                // no response will arrive. A permanent leak here would
                // make Task A steal the next application CPR response.
                if cpr_incremented {
                    match parser_for_stdout.lock() {
                        Ok(mut p) => {
                            // claim_cpr_response decrements cpr_pending.
                            p.state_mut().claim_cpr_response();
                        }
                        Err(poison_err) => {
                            // If the rollback lock itself fails we can't
                            // recover — the parser mutex is poisoned AND
                            // cpr_pending is permanently elevated by 1,
                            // which would make Task A steal the next real
                            // application CPR response (from atuin,
                            // crossterm, etc.) and corrupt downstream
                            // parsing. Proxy state is corrupted; cascade
                            // into the same break pattern as the parser
                            // mutex poison sites earlier in this loop and
                            // exit Task B immediately.
                            tracing::error!(
                                "parser mutex poisoned during CPR rollback: {poison_err} \
                                 — cpr_pending permanently elevated, exiting Task B"
                            );
                            break;
                        }
                    }
                }
                tracing::debug!("stdout write/flush failed: {e}");
                break;
            }

            // Check if shell reported a buffer update via OSC 7770.
            // Trigger suggestions here (Task B) instead of Task A to ensure
            // we have the shell's updated buffer, fixing the stale-buffer bug.
            let buffer_dirty = {
                match parser_for_stdout.lock() {
                    Ok(mut p) => p.state_mut().take_buffer_dirty(),
                    Err(e) => {
                        tracing::warn!("parser mutex poisoned in stdout task: {e}");
                        break;
                    }
                }
            };

            if buffer_dirty {
                let mut render_buf = Vec::new();
                {
                    let mut h = match handler_for_stdout.lock() {
                        Ok(h) => h,
                        Err(e) => {
                            tracing::warn!("handler mutex poisoned in stdout task: {e}");
                            break;
                        }
                    };
                    if h.has_pending_trigger() {
                        h.clear_trigger_request();
                        if h.auto_trigger_enabled() {
                            h.trigger(&parser_for_stdout, &mut render_buf);
                        }
                    } else if delay_ms > 0
                        && h.auto_trigger_enabled()
                        && !h.is_debounce_suppressed()
                    {
                        debounce_notify_b.notify_one();
                    }
                }
                if !render_buf.is_empty() {
                    let mut stdout = std::io::stdout().lock();
                    let _ = stdout.write_all(&render_buf);
                    let _ = stdout.flush();
                }
            }

            // CD chaining: trigger suggestions on CWD change (OSC 7), gated by auto_trigger.
            let cwd_dirty = {
                match parser_for_stdout.lock() {
                    Ok(mut p) => p.state_mut().take_cwd_dirty(),
                    Err(e) => {
                        tracing::warn!("parser mutex poisoned in stdout task: {e}");
                        break;
                    }
                }
            };

            if cwd_dirty {
                let mut render_buf = Vec::new();
                {
                    let mut h = match handler_for_stdout.lock() {
                        Ok(h) => h,
                        Err(e) => {
                            tracing::warn!("handler mutex poisoned in stdout task: {e}");
                            break;
                        }
                    };
                    if h.auto_trigger_enabled() {
                        h.trigger(&parser_for_stdout, &mut render_buf);
                    }
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
                    let mut h = match handler_for_stdout.lock() {
                        Ok(h) => h,
                        Err(e) => {
                            tracing::warn!("handler mutex poisoned in stdout task: {e}");
                            break;
                        }
                    };
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
                        match parser.lock() {
                            Ok(mut p) => {
                                p.state_mut().update_dimensions(size.rows, size.cols);
                            }
                            Err(e) => {
                                tracing::warn!("parser mutex poisoned on SIGWINCH: {e}");
                            }
                        }
                        // Re-render popup if visible — buffer output under the
                        // handler lock, then write to stdout after releasing it.
                        // This avoids holding handler+stdout simultaneously (the
                        // same pattern every other code path uses).
                        let mut render_buf = Vec::new();
                        match handler.lock() {
                            Ok(mut h) => {
                                h.handle_resize(&parser, &mut render_buf);
                            }
                            Err(e) => {
                                tracing::warn!("handler mutex poisoned on SIGWINCH: {e}");
                            }
                        }
                        if !render_buf.is_empty() {
                            let mut stdout = std::io::stdout().lock();
                            let _ = stdout.write_all(&render_buf);
                            let _ = stdout.flush();
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
    if let Some(h) = config_watcher_handle {
        h.shutdown();
    }

    // Note: we do NOT clean up `tmux setenv GHOST_COMPLETE_ACTIVE` on exit.
    // Multiple panes share the session env, so the first pane to exit would
    // remove it for all others. Leaving it set is harmless — init.zsh's tmux
    // branch uses PPID + GHOST_COMPLETE_PANE, not this variable.

    // Abort any in-flight dynamic generator task and flush frecency.
    match handler.lock() {
        Ok(mut h) => {
            h.abort_dynamic_task();
            h.flush_frecency();
        }
        Err(e) => {
            tracing::warn!("handler mutex poisoned at shutdown, frecency data not flushed: {e}")
        }
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
            let mut h = match handler.lock() {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!("debounce skipped (handler lock poisoned): {e}");
                    continue;
                }
            };
            if h.is_debounce_suppressed() || !h.auto_trigger_enabled() {
                continue;
            }
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
            let mut h = match handler.lock() {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!("dynamic merge skipped (handler lock poisoned): {e}");
                    continue;
                }
            };
            h.try_merge_dynamic(&parser, &mut render_buf);
        }
        if !render_buf.is_empty() {
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(&render_buf);
            let _ = stdout.flush();
        }
    }
}

/// Returns true if ghost-complete should replace itself with a plain shell
/// because multi-terminal support is disabled and we're not on Ghostty.
pub fn should_fallback_to_shell(
    terminal: &gc_terminal::Terminal,
    multi_terminal_enabled: bool,
) -> bool {
    // All known terminals work without the experimental flag.
    // Only Unknown terminals require multi_terminal = true.
    matches!(terminal, gc_terminal::Terminal::Unknown(_)) && !multi_terminal_enabled
}

#[cfg(test)]
mod tests {
    use super::*;
    use gc_terminal::Terminal;

    #[test]
    fn test_known_terminals_never_fall_back() {
        // All known terminals work without the experimental flag
        let known = [
            Terminal::Ghostty,
            Terminal::Kitty,
            Terminal::WezTerm,
            Terminal::Alacritty,
            Terminal::Rio,
            Terminal::ITerm2,
            Terminal::TerminalApp,
        ];
        for terminal in &known {
            assert!(
                !should_fallback_to_shell(terminal, false),
                "{terminal} should not fall back without multi_terminal flag"
            );
            assert!(
                !should_fallback_to_shell(terminal, true),
                "{terminal} should not fall back with multi_terminal flag"
            );
        }
    }

    #[test]
    fn test_unknown_falls_back_without_flag() {
        assert!(should_fallback_to_shell(
            &Terminal::Unknown("foot".into()),
            false
        ));
    }

    #[test]
    fn test_unknown_runs_with_flag() {
        assert!(!should_fallback_to_shell(
            &Terminal::Unknown("foot".into()),
            true
        ));
    }
}
