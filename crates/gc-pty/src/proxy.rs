use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use gc_parser::{CprOwner, TerminalParser};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{mpsc, Notify};

use gc_config::GhostConfig;

use gc_overlay::{parse_style, PopupTheme};
use gc_suggest::spec_dirs::resolve_spec_dirs;

use crate::config_watch::spawn_config_watcher;
use crate::handler::{InputHandler, Keybindings, OverlayWriteTicket, TriggerPrepared};
use crate::input::{KeyEvent, KeyParser};
use crate::resize::{get_terminal_size, resize_pty};
use crate::spawn::{spawn_shell, SpawnedShell};

/// Upper bound on how long a queued CPR entry may sit before we prune it.
/// A misbehaving terminal that silently drops `CSI 6n` would otherwise leak
/// queue entries forever. A late response after prune lands as
/// `CprAction::DropEmpty` and is forwarded defensively, which is why the
/// threshold is generous.
const CPR_STALE_THRESHOLD: Duration = Duration::from_secs(30);
const ASYNC_OVERLAY_GATE_RETRY: Duration = Duration::from_millis(10);

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
        feedback_loading_on: parse_style(&resolved_theme.feedback_loading)
            .context("invalid theme.feedback_loading style")?,
        feedback_empty_on: parse_style(&resolved_theme.feedback_empty)
            .context("invalid theme.feedback_empty style")?,
        feedback_error_on: parse_style(&resolved_theme.feedback_error)
            .context("invalid theme.feedback_error style")?,
        match_highlight_on: parse_style(&resolved_theme.match_highlight)
            .context("invalid theme.match_highlight style")?,
        item_text_on: parse_style(&resolved_theme.item_text)
            .context("invalid theme.item_text style")?,
        scrollbar_on: parse_style(&resolved_theme.scrollbar)
            .context("invalid theme.scrollbar style")?,
        border_on: parse_style(&resolved_theme.border).context("invalid theme.border style")?,
        borders: config.popup.borders,
        spinner: config.popup.spinner,
        show_provider_errors: config.popup.show_provider_errors,
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
            .with_feedback_dismiss_ms(config.popup.feedback_dismiss_ms)
            .with_trigger_chars(&config.trigger.auto_chars)
            .with_auto_trigger(config.trigger.auto_trigger)
            .with_render_block_ms(config.popup.render_block_ms as u64)
            .with_suggest_config(
                config.suggest.max_results,
                config.suggest.providers.commands,
                config.suggest.max_history_results,
                config.suggest.providers.filesystem,
                config.suggest.providers.specs,
                config.suggest.providers.git,
                config.suggest.providers.js_runtime,
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
    let debounce_state = Arc::new(Mutex::new(DebounceState::default()));
    // Keeps debounce generation checks atomic with the parser snapshot they render from.
    let trigger_snapshot_gate = Arc::new(Mutex::new(()));
    let delay_ms = config.trigger.delay_ms;

    let debounce_handle = if delay_ms > 0 {
        let notify = Arc::clone(&debounce_notify);
        let debounce_state_d = Arc::clone(&debounce_state);
        let trigger_snapshot_gate_d = Arc::clone(&trigger_snapshot_gate);
        let handler_d = Arc::clone(&handler);
        let parser_d = Arc::clone(&parser);
        Some(tokio::spawn(async move {
            debounce_loop(
                notify,
                debounce_state_d,
                trigger_snapshot_gate_d,
                handler_d,
                parser_d,
                delay_ms,
            )
            .await;
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
    let debounce_state_for_merge = Arc::clone(&debounce_state);
    let merge_handle = tokio::spawn(async move {
        dynamic_merge_loop(
            dynamic_notify,
            handler_for_merge,
            parser_for_merge,
            debounce_state_for_merge,
        )
        .await;
    });

    let feedback_notify = {
        let h = match handler.lock() {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("handler mutex poisoned during feedback setup: {e}");
                anyhow::bail!("handler mutex poisoned during feedback setup — cannot start proxy");
            }
        };
        h.feedback_tick_notify()
    };
    let handler_for_feedback = Arc::clone(&handler);
    let debounce_state_for_feedback = Arc::clone(&debounce_state);
    let feedback_handle = tokio::spawn(async move {
        feedback_tick_loop(
            feedback_notify,
            handler_for_feedback,
            debounce_state_for_feedback,
        )
        .await;
    });

    // Channel to signal that one of the I/O tasks has finished
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

    // Task A: stdin → PTY (user keystrokes to shell, with popup interception)
    let stdin_shutdown = shutdown_tx.clone();
    let mut pty_writer = writer;
    let parser_for_stdin = Arc::clone(&parser);
    let handler_for_stdin = Arc::clone(&handler);
    let debounce_state_for_stdin = Arc::clone(&debounce_state);
    let trigger_snapshot_gate_a = Arc::clone(&trigger_snapshot_gate);
    let stdin_handle = tokio::task::spawn_blocking(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 4096];
        let mut key_parser = KeyParser::new();
        'stdin: loop {
            let n = match stdin.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };

            let keys = key_parser.parse(&buf[..n]);
            for key in &keys {
                // CPR (Cursor Position Report) responses arrive here
                // from the real terminal. If we sent the request, consume
                // it for cursor sync. Otherwise forward it through the
                // PTY so programs like atuin/crossterm receive it.
                if let crate::input::KeyEvent::CursorPositionReport(row, col) = key {
                    let action = {
                        let mut p = match parser_for_stdin.lock() {
                            Ok(p) => p,
                            Err(e) => {
                                tracing::warn!("parser mutex poisoned in stdin task: {e}");
                                break 'stdin;
                            }
                        };
                        dispatch_cpr_response(p.state_mut(), *row, *col)
                    };
                    match action {
                        CprAction::SyncOurs(r, c) => {
                            // Deliberate re-acquire after the claim-only
                            // lock above. Narrowing the hold keeps parser
                            // contention off the dispatch decision; the
                            // sync below is the only path that needs the
                            // lock again.
                            let mut p = match parser_for_stdin.lock() {
                                Ok(p) => p,
                                Err(e) => {
                                    tracing::warn!("parser mutex poisoned in stdin task: {e}");
                                    break 'stdin;
                                }
                            };
                            let state = p.state_mut();
                            if state.validate_cpr_coordinates(r, c) {
                                tracing::debug!(
                                    row = r,
                                    col = c,
                                    "CPR response — syncing cursor position (ours)"
                                );
                                state.set_cursor_from_report(r, c);
                            } else {
                                // Cached screen dimensions are stale (e.g. a
                                // SIGWINCH was missed or the proxy was
                                // launched into a tty whose size later
                                // changed without a kernel notification).
                                // Re-query the real terminal size and update
                                // the parser, then re-validate. If the new
                                // size accommodates the report, accept it
                                // and let the deficit reset on the next
                                // trigger. Otherwise drop the stale deficit
                                // anyway — accumulating it under a wrong
                                // size makes the popup drift further with
                                // every render.
                                let (old_rows, old_cols) = state.screen_dimensions();
                                let recovered = match get_terminal_size() {
                                    Ok(size) if (size.rows, size.cols) != (old_rows, old_cols) => {
                                        state.update_dimensions(size.rows, size.cols);
                                        // Only the parser dimensions are
                                        // updated here — `master` lives in
                                        // the outer task and the PTY-resize
                                        // path is owned by the SIGWINCH
                                        // handler. Updating the parser is
                                        // sufficient to unblock CPR
                                        // validation and stop the popup
                                        // from drifting; the shell-side
                                        // PTY size will reconcile on the
                                        // next real SIGWINCH. The next
                                        // popup render that uses the new
                                        // dimensions only writes through
                                        // the terminal (not the PTY), so
                                        // there is no hard correctness
                                        // dependency on the PTY size for
                                        // this branch.
                                        tracing::warn!(
                                            old_rows,
                                            old_cols,
                                            new_rows = size.rows,
                                            new_cols = size.cols,
                                            "CPR row/col exceeded cached screen — re-queried \
                                             terminal size and updated parser dimensions"
                                        );
                                        if state.validate_cpr_coordinates(r, c) {
                                            state.set_cursor_from_report(r, c);
                                            true
                                        } else {
                                            false
                                        }
                                    }
                                    Ok(_) => false,
                                    Err(e) => {
                                        tracing::warn!(
                                            "failed to re-query terminal size on CPR mismatch: {e}"
                                        );
                                        false
                                    }
                                };
                                if !recovered {
                                    let (screen_rows, screen_cols) = state.screen_dimensions();
                                    tracing::warn!(
                                        row = r,
                                        col = c,
                                        screen_rows,
                                        screen_cols,
                                        "CPR coordinates out of screen bounds — dropping \
                                         stale overlay scroll deficit defensively"
                                    );
                                    drop(p);
                                    if let Ok(mut h) = handler_for_stdin.lock() {
                                        h.invalidate_overlay_scroll_deficit();
                                    }
                                }
                            }
                        }
                        CprAction::ForwardToPty(r, c) => {
                            tracing::debug!(
                                row = r,
                                col = c,
                                "CPR response — forwarding to PTY (shell)"
                            );
                            let cpr = format!("\x1b[{r};{c}R");
                            if write_pty_or_shutdown(
                                pty_writer.as_mut(),
                                cpr.as_bytes(),
                                "forward shell CPR response",
                            )
                            .is_err()
                            {
                                break 'stdin;
                            }
                        }
                        CprAction::DropEmpty(r, c) => {
                            tracing::warn!(
                                row = r,
                                col = c,
                                "CPR response with empty queue — forwarding defensively"
                            );
                            let cpr = format!("\x1b[{r};{c}R");
                            if write_pty_or_shutdown(
                                pty_writer.as_mut(),
                                cpr.as_bytes(),
                                "forward defensive CPR response",
                            )
                            .is_err()
                            {
                                break 'stdin;
                            }
                        }
                    }
                    continue;
                }

                // Handler writes popup rendering into a buffer instead of
                // locking stdout for the entire loop (which would deadlock
                // with Task B's stdout writes).
                let mut render_buf = Vec::new();
                let (forward, render_ticket) = {
                    let _snapshot_guard = match trigger_snapshot_gate_a.lock() {
                        Ok(guard) => guard,
                        Err(e) => {
                            tracing::warn!("trigger snapshot gate poisoned in stdin task: {e}");
                            break 'stdin;
                        }
                    };
                    let mut h = match handler_for_stdin.lock() {
                        Ok(h) => h,
                        Err(e) => {
                            tracing::warn!("handler mutex poisoned in stdin task: {e}");
                            break 'stdin;
                        }
                    };
                    let forward = h.process_key(key, &parser_for_stdin, &mut render_buf);
                    let invalidates_debounce =
                        !forward.is_empty() && forwarded_input_invalidates_debounce(key);
                    if invalidates_debounce {
                        match debounce_state_for_stdin.lock() {
                            Ok(mut state) => state.invalidate_for_forwarded_input(),
                            Err(e) => {
                                tracing::warn!("debounce state mutex poisoned in stdin task: {e}");
                                break 'stdin;
                            }
                        }
                    }
                    (forward, h.overlay_write_ticket())
                };
                if !render_buf.is_empty() {
                    if let Err(e) =
                        write_overlay_if_current(&handler_for_stdin, render_ticket, &render_buf)
                    {
                        tracing::debug!("Task A overlay write/flush failed: {e}");
                        break 'stdin;
                    }
                }
                if !forward.is_empty()
                    && write_pty_or_shutdown(
                        pty_writer.as_mut(),
                        &forward,
                        "forward terminal input",
                    )
                    .is_err()
                {
                    break 'stdin;
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
    let debounce_state_b = Arc::clone(&debounce_state);
    let trigger_snapshot_gate_b = Arc::clone(&trigger_snapshot_gate);
    let stdout_handle = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 8192];
        let mut display_epoch = 0_u64;
        let mut deferred_buffer_dirty: Option<DeferredBufferDirty> = None;
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break, // PTY closed
                Ok(n) => n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };

            // Feed bytes through the VT parser to track terminal state
            let (
                needs_cpr,
                display_dirty,
                viewport_scrolls,
                latest_buffer_epoch,
                latest_buffer_generation,
            ) = {
                let _snapshot_guard = match trigger_snapshot_gate_b.lock() {
                    Ok(guard) => guard,
                    Err(e) => {
                        tracing::warn!("trigger snapshot gate poisoned in stdout task: {e}");
                        break;
                    }
                };
                let mut p = match parser_for_stdout.lock() {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!("parser mutex poisoned in stdout task: {e}");
                        break;
                    }
                };
                let mut debounce_state = match debounce_state_b.lock() {
                    Ok(state) => state,
                    Err(e) => {
                        tracing::warn!("debounce state mutex poisoned in stdout task: {e}");
                        break;
                    }
                };
                let events = process_pty_read(&mut p, &buf[..n], &mut display_epoch, || {
                    debounce_state.next_buffer_generation()
                });
                (
                    events.needs_cpr,
                    events.display_dirty,
                    events.viewport_scrolls,
                    events.latest_buffer_epoch,
                    events.latest_buffer_generation,
                )
            };

            // Lock ordering: take the parser lock to enqueue Ours, drop
            // it BEFORE acquiring stdout. Task A holds parser briefly to
            // pop the queue head; nesting (stdout → parser) here would
            // deadlock the moment Task A tried to acquire parser while
            // Task B held stdout.
            let cpr_token = if needs_cpr {
                match parser_for_stdout.lock() {
                    Ok(mut p) => Some(p.state_mut().enqueue_cpr(CprOwner::Ours)),
                    Err(e) => {
                        tracing::warn!(
                            "parser mutex poisoned before CPR enqueue: {e} \
                             — skipping CPR"
                        );
                        None
                    }
                }
            } else {
                None
            };
            // If we couldn't enqueue (poisoned mutex), don't emit the
            // CSI 6n — sending without a queue entry would make Task A
            // forward our response to the PTY.
            let send_cpr = cpr_token.is_some();

            let mut cleanup = Vec::new();
            if display_dirty || viewport_scrolls > 0 {
                match handler_for_stdout.lock() {
                    Ok(mut h) => {
                        h.handle_terminal_output(&mut cleanup, display_dirty, viewport_scrolls);
                    }
                    Err(e) => {
                        tracing::warn!(
                            "handler mutex poisoned before terminal output cleanup: {e}"
                        );
                        break;
                    }
                }
            }

            let write_result: std::io::Result<()> = {
                let mut stdout = std::io::stdout().lock();
                stdout
                    .write_all(&cleanup)
                    .and_then(|()| stdout.write_all(&buf[..n]))
                    .and_then(|()| {
                        if send_cpr {
                            tracing::debug!("sending CPR request (CSI 6n)");
                            stdout.write_all(b"\x1b[6n").and_then(|()| stdout.flush())
                        } else {
                            stdout.flush()
                        }
                    })
            };

            if let Err(e) = write_result {
                // Rollback: the CSI 6n didn't reach the terminal (or we
                // can't prove it did), so no response will arrive. Remove
                // the orphan entry before it shifts dispatch alignment
                // for every subsequent CPR.
                if let Some(token) = cpr_token {
                    match parser_for_stdout.lock() {
                        Ok(mut p) => {
                            if !p.state_mut().rollback_cpr(token) {
                                // Benign race: write reported failure but the
                                // bytes already reached the terminal, which
                                // responded; Task A claimed the entry before
                                // we got here. No orphan, no action needed.
                                tracing::debug!(
                                    "CPR rollback no-op — entry already claimed by Task A"
                                );
                            }
                        }
                        Err(poison_err) => {
                            tracing::error!(
                                "parser mutex poisoned during CPR rollback: {poison_err} \
                                 — orphan entry leaked, exiting Task B"
                            );
                            break;
                        }
                    }
                }
                tracing::debug!("Task B stdout write/flush failed: {e}");
                break;
            }

            {
                let dropped = match parser_for_stdout.lock() {
                    Ok(mut p) => p.state_mut().prune_stale_cpr(CPR_STALE_THRESHOLD),
                    Err(e) => {
                        tracing::warn!("parser mutex poisoned during CPR prune: {e}");
                        0
                    }
                };
                if dropped > 0 {
                    tracing::warn!(dropped, "pruned stale CPR queue entries");
                }
            }

            // Handle shell buffer updates in Task B instead of Task A so trigger/debounce
            // decisions see the parser's updated buffer. OSC-only reports may be deferred
            // until a later display epoch to avoid rendering before the terminal redraws.
            if let Some(latest_buffer_epoch) = latest_buffer_epoch {
                let Some(buffer_generation) = latest_buffer_generation else {
                    tracing::warn!("buffer report missing generation in stdout task");
                    break;
                };
                let display_after_latest_buffer = display_epoch > latest_buffer_epoch;
                let action = {
                    let mut h = match handler_for_stdout.lock() {
                        Ok(h) => h,
                        Err(e) => {
                            tracing::warn!("handler mutex poisoned in stdout task: {e}");
                            break;
                        }
                    };
                    let had_pending_trigger = h.has_pending_trigger();
                    let action = buffer_dirty_action(
                        had_pending_trigger,
                        delay_ms,
                        h.auto_trigger_enabled(),
                        h.is_debounce_suppressed(),
                        display_after_latest_buffer,
                    );
                    if had_pending_trigger {
                        h.clear_trigger_request();
                    }
                    action
                };

                let resolved_action = {
                    let mut state = match debounce_state_b.lock() {
                        Ok(state) => state,
                        Err(e) => {
                            tracing::warn!("debounce state mutex poisoned in stdout task: {e}");
                            break;
                        }
                    };
                    apply_buffer_dirty_action(
                        &mut deferred_buffer_dirty,
                        &mut state,
                        latest_buffer_epoch,
                        buffer_generation,
                        action,
                    )
                };

                match resolved_action {
                    ResolvedBufferDirtyAction::Trigger => {
                        let mut render_buf = Vec::new();
                        let render_ticket = {
                            let mut h = match handler_for_stdout.lock() {
                                Ok(h) => h,
                                Err(e) => {
                                    tracing::warn!("handler mutex poisoned in stdout task: {e}");
                                    break;
                                }
                            };
                            h.trigger(&parser_for_stdout, &mut render_buf);
                            h.overlay_write_ticket()
                        };
                        if !render_buf.is_empty() {
                            if let Err(e) = write_overlay_if_current(
                                &handler_for_stdout,
                                render_ticket,
                                &render_buf,
                            ) {
                                tracing::debug!("Task B overlay write/flush failed: {e}");
                                break;
                            }
                        }
                    }
                    ResolvedBufferDirtyAction::NotifyDebounce => debounce_notify_b.notify_one(),
                    ResolvedBufferDirtyAction::None => {}
                }
            }

            if deferred_buffer_dirty.is_some_and(|pending| display_epoch > pending.display_epoch) {
                let (auto_trigger_enabled, debounce_suppressed) = {
                    let h = match handler_for_stdout.lock() {
                        Ok(h) => h,
                        Err(e) => {
                            tracing::warn!("handler mutex poisoned in stdout task: {e}");
                            break;
                        }
                    };
                    (h.auto_trigger_enabled(), h.is_debounce_suppressed())
                };
                let resolved_action = {
                    let mut state = match debounce_state_b.lock() {
                        Ok(state) => state,
                        Err(e) => {
                            tracing::warn!("debounce state mutex poisoned in stdout task: {e}");
                            break;
                        }
                    };
                    resolve_deferred_buffer_dirty(
                        &mut deferred_buffer_dirty,
                        &mut state,
                        display_epoch,
                        auto_trigger_enabled,
                        debounce_suppressed,
                    )
                };

                match resolved_action {
                    ResolvedBufferDirtyAction::Trigger => {
                        let mut render_buf = Vec::new();
                        let render_ticket = {
                            let mut h = match handler_for_stdout.lock() {
                                Ok(h) => h,
                                Err(e) => {
                                    tracing::warn!("handler mutex poisoned in stdout task: {e}");
                                    break;
                                }
                            };
                            h.trigger(&parser_for_stdout, &mut render_buf);
                            h.overlay_write_ticket()
                        };
                        if !render_buf.is_empty() {
                            if let Err(e) = write_overlay_if_current(
                                &handler_for_stdout,
                                render_ticket,
                                &render_buf,
                            ) {
                                tracing::debug!("Task B deferred overlay write/flush failed: {e}");
                                break;
                            }
                        }
                    }
                    ResolvedBufferDirtyAction::NotifyDebounce => debounce_notify_b.notify_one(),
                    ResolvedBufferDirtyAction::None => {}
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
                let auto_trigger_enabled = {
                    let h = match handler_for_stdout.lock() {
                        Ok(h) => h,
                        Err(e) => {
                            tracing::warn!("handler mutex poisoned in stdout task: {e}");
                            break;
                        }
                    };
                    h.auto_trigger_enabled()
                };
                let cwd_deferred = if auto_trigger_enabled {
                    let mut state = match debounce_state_b.lock() {
                        Ok(state) => state,
                        Err(e) => {
                            tracing::warn!("debounce state mutex poisoned in stdout task: {e}");
                            break;
                        }
                    };
                    defer_cwd_trigger_until_buffer_display(
                        &mut deferred_buffer_dirty,
                        &mut state,
                        latest_buffer_epoch,
                        latest_buffer_generation,
                        display_epoch,
                    )
                } else {
                    false
                };
                if auto_trigger_enabled && !cwd_deferred {
                    let mut render_buf = Vec::new();
                    let render_ticket = {
                        let mut h = match handler_for_stdout.lock() {
                            Ok(h) => h,
                            Err(e) => {
                                tracing::warn!("handler mutex poisoned in stdout task: {e}");
                                break;
                            }
                        };
                        h.trigger(&parser_for_stdout, &mut render_buf);
                        h.overlay_write_ticket()
                    };
                    if !render_buf.is_empty() {
                        if let Err(e) = write_overlay_if_current(
                            &handler_for_stdout,
                            render_ticket,
                            &render_buf,
                        ) {
                            tracing::debug!("Task B overlay write/flush failed: {e}");
                            break;
                        }
                    }
                }
            }

            // Poll for dynamic (script generator) results — non-blocking.
            {
                match locked_async_overlay_gate_decision(&debounce_state_b, "stdout task") {
                    Some(AsyncOverlayGateDecision::Render) => {
                        let mut render_buf = Vec::new();
                        let render_ticket = {
                            let mut h = match handler_for_stdout.lock() {
                                Ok(h) => h,
                                Err(e) => {
                                    tracing::warn!("handler mutex poisoned in stdout task: {e}");
                                    break;
                                }
                            };
                            h.try_merge_dynamic(&parser_for_stdout, &mut render_buf);
                            h.overlay_write_ticket()
                        };
                        if !render_buf.is_empty() {
                            if let Err(e) = write_overlay_if_current(
                                &handler_for_stdout,
                                render_ticket,
                                &render_buf,
                            ) {
                                tracing::debug!("Task B overlay write/flush failed: {e}");
                                break;
                            }
                        }
                    }
                    Some(AsyncOverlayGateDecision::Blocked) => {}
                    None => {
                        break;
                    }
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
    let mut sigterm =
        signal(SignalKind::terminate()).context("failed to register SIGTERM handler")?;
    let mut sighup = signal(SignalKind::hangup()).context("failed to register SIGHUP handler")?;

    // Wait for either an I/O task to finish or a signal
    let mut signal_shutdown = false;
    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                tracing::debug!("I/O task finished, shutting down");
                break;
            }
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM, shutting down");
                signal_shutdown = true;
                break;
            }
            _ = sighup.recv() => {
                tracing::info!("received SIGHUP, shutting down");
                signal_shutdown = true;
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
                        // Dismiss popup if visible, then write cleanup through
                        // the epoch gate so stale resize cleanup cannot land
                        // after newer shell output invalidated popup ownership.
                        let mut render_buf = Vec::new();
                        let render_ticket = match handler.lock() {
                            Ok(mut h) => {
                                h.handle_resize(&parser, &mut render_buf);
                                Some(h.overlay_write_ticket())
                            }
                            Err(e) => {
                                tracing::warn!("handler mutex poisoned on SIGWINCH: {e}");
                                None
                            }
                        };
                        if !render_buf.is_empty() {
                            let Some(render_ticket) = render_ticket else {
                                continue;
                            };
                            if let Err(e) =
                                write_overlay_if_current(&handler, render_ticket, &render_buf)
                            {
                                tracing::debug!("signal overlay write/flush failed: {e}");
                                break;
                            }
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
    feedback_handle.abort();
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

    // Drop the raw-mode guard eagerly on signal shutdown so the terminal is
    // returned to cooked mode *before* the bounded `try_wait` loop. Holding
    // the guard across the 2 s deadline leaves the user staring at a broken
    // prompt while we wait for the shell to exit. On the normal path the
    // guard falls out of scope at function return, which is fine — `child.
    // wait()` there blocks until the shell actually closes the PTY.
    if signal_shutdown {
        drop(_raw_guard);
    }

    // Wait for child and get exit status.
    //
    // On signal-driven shutdown, the shell may be blocked on a read of the
    // inherited master PTY fd. A plain `wait()` would hang forever. Poll
    // `try_wait` with a bounded deadline, then escalate to `kill()` if the
    // shell hasn't exited on its own.
    let exit_code = if signal_shutdown {
        wait_with_timeout(child.as_mut(), Duration::from_secs(2))
    } else {
        let status = child.wait().context("failed to wait for shell process")?;
        status.exit_code().try_into().unwrap_or(1)
    };

    Ok(exit_code)
}

/// Poll `try_wait` until `deadline`, then `kill()` and re-poll with a bounded
/// reap deadline. Returns the shell's exit code, or a signal-style
/// `128 + SIGTERM = 143` if we had to kill it (or if the child is still alive
/// after the reap deadline).
///
/// Every wait path is bounded — no plain blocking `wait()` on the signal path,
/// because the shell can be stuck on an inherited PTY fd and hang forever.
fn wait_with_timeout(
    child: &mut (dyn portable_pty::Child + Send + Sync),
    deadline: Duration,
) -> i32 {
    let poll_interval = Duration::from_millis(50);
    let reap_deadline = Duration::from_millis(500);
    let pid = child.process_id();

    if let Some(code) = poll_until(child, deadline, poll_interval) {
        return code;
    }

    if let Err(e) = child.kill() {
        tracing::warn!("failed to kill shell on signal shutdown: {e}");
    }

    if let Some(code) = poll_until(child, reap_deadline, poll_interval) {
        return code;
    }

    tracing::error!(
        "shell pid={:?} survived kill and {}ms reap deadline; proxy exiting with 143, process may be orphaned",
        pid,
        reap_deadline.as_millis()
    );
    143
}

/// Poll `try_wait` until the child exits or `deadline` elapses. Returns
/// `Some(exit_code)` if the child reaped before the deadline, `None` otherwise.
fn poll_until(
    child: &mut (dyn portable_pty::Child + Send + Sync),
    deadline: Duration,
    poll_interval: Duration,
) -> Option<i32> {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.exit_code().try_into().unwrap_or(1)),
            Ok(None) => {
                if start.elapsed() >= deadline {
                    return None;
                }
                std::thread::sleep(poll_interval);
            }
            Err(e) => {
                tracing::warn!("try_wait failed during signal shutdown: {e}");
                return None;
            }
        }
    }
}

fn forwarded_input_invalidates_debounce(key: &KeyEvent) -> bool {
    !matches!(key, KeyEvent::CursorPositionReport(_, _))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PtyReadEvents {
    needs_cpr: bool,
    display_dirty: bool,
    viewport_scrolls: u16,
    latest_buffer_epoch: Option<u64>,
    latest_buffer_generation: Option<u64>,
}

fn process_pty_read(
    parser: &mut TerminalParser,
    bytes: &[u8],
    display_epoch: &mut u64,
    mut next_buffer_generation: impl FnMut() -> u64,
) -> PtyReadEvents {
    let mut events = PtyReadEvents::default();
    for byte in bytes {
        parser.process_bytes(std::slice::from_ref(byte));
        let state = parser.state_mut();

        events.needs_cpr |= state.take_cursor_sync_requested();

        let display_dirty = state.take_display_dirty();
        let viewport_scrolls = state.take_viewport_scroll_count();
        events.display_dirty |= display_dirty;
        events.viewport_scrolls = events.viewport_scrolls.saturating_add(viewport_scrolls);
        if display_dirty || viewport_scrolls > 0 {
            *display_epoch = display_epoch.saturating_add(1);
        }

        if state.take_buffer_dirty() {
            events.latest_buffer_epoch = Some(*display_epoch);
            events.latest_buffer_generation = Some(next_buffer_generation());
        }
    }
    events
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeferredBufferDirtyAction {
    TriggerNow,
    NotifyDebounce,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct DebounceState {
    current_generation: u64,
    notified_generation: Option<u64>,
    deferred_generation: Option<u64>,
}

impl DebounceState {
    fn next_buffer_generation(&mut self) -> u64 {
        self.current_generation += 1;
        self.current_generation
    }

    fn invalidate_for_forwarded_input(&mut self) {
        if self.deferred_generation.is_some() {
            return;
        }
        self.current_generation += 1;
    }

    fn notify_generation(&mut self, generation: u64) {
        self.notified_generation = Some(generation);
        if self.deferred_generation == Some(generation) {
            self.deferred_generation = None;
        }
    }

    fn defer_generation(&mut self, generation: u64) {
        self.deferred_generation = Some(generation);
    }

    fn clear_deferred_generation(&mut self, generation: u64) {
        if self.deferred_generation == Some(generation) {
            self.deferred_generation = None;
        }
    }

    fn latest_notified_generation(&self) -> Option<u64> {
        self.notified_generation
    }

    fn can_fire_generation(&self, generation: u64) -> bool {
        self.current_generation == generation
            && self.notified_generation == Some(generation)
            && self.deferred_generation.is_none()
    }

    fn has_deferred_generation(&self) -> bool {
        self.deferred_generation.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeferredBufferDirty {
    action: DeferredBufferDirtyAction,
    display_epoch: u64,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BufferDirtyAction {
    Trigger,
    Debounce,
    DeferUntilDisplay(DeferredBufferDirtyAction),
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolvedBufferDirtyAction {
    Trigger,
    NotifyDebounce,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DebounceFireDecision {
    Fire,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DebounceContinuationDecision {
    Apply,
    Drop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AsyncOverlayGateDecision {
    Render,
    Blocked,
}

fn debounce_fire_decision(state: &DebounceState, generation: u64) -> DebounceFireDecision {
    if state.can_fire_generation(generation) {
        DebounceFireDecision::Fire
    } else {
        DebounceFireDecision::Skip
    }
}

fn debounce_continuation_decision(
    state: &DebounceState,
    generation: u64,
) -> DebounceContinuationDecision {
    if state.can_fire_generation(generation) {
        DebounceContinuationDecision::Apply
    } else {
        DebounceContinuationDecision::Drop
    }
}

fn async_overlay_gate_decision(state: &DebounceState) -> AsyncOverlayGateDecision {
    if state.has_deferred_generation() {
        AsyncOverlayGateDecision::Blocked
    } else {
        AsyncOverlayGateDecision::Render
    }
}

fn locked_async_overlay_gate_decision(
    debounce_state: &Arc<Mutex<DebounceState>>,
    source: &'static str,
) -> Option<AsyncOverlayGateDecision> {
    match debounce_state.lock() {
        Ok(state) => Some(async_overlay_gate_decision(&state)),
        Err(e) => {
            tracing::warn!(
                source = source,
                "async overlay gate skipped (debounce state lock poisoned): {e}"
            );
            None
        }
    }
}

fn buffer_dirty_action(
    has_pending_trigger: bool,
    delay_ms: u64,
    auto_trigger_enabled: bool,
    debounce_suppressed: bool,
    display_changed: bool,
) -> BufferDirtyAction {
    if !auto_trigger_enabled {
        return BufferDirtyAction::Ignore;
    }
    if has_pending_trigger && !display_changed {
        return BufferDirtyAction::DeferUntilDisplay(DeferredBufferDirtyAction::TriggerNow);
    }
    if has_pending_trigger {
        return BufferDirtyAction::Trigger;
    }
    if debounce_suppressed {
        return BufferDirtyAction::Ignore;
    }
    if delay_ms == 0 && !display_changed {
        BufferDirtyAction::DeferUntilDisplay(DeferredBufferDirtyAction::TriggerNow)
    } else if delay_ms == 0 {
        BufferDirtyAction::Trigger
    } else if !display_changed {
        BufferDirtyAction::DeferUntilDisplay(DeferredBufferDirtyAction::NotifyDebounce)
    } else {
        BufferDirtyAction::Debounce
    }
}

fn apply_buffer_dirty_action(
    deferred: &mut Option<DeferredBufferDirty>,
    debounce_state: &mut DebounceState,
    buffer_epoch: u64,
    buffer_generation: u64,
    action: BufferDirtyAction,
) -> ResolvedBufferDirtyAction {
    match action {
        BufferDirtyAction::Trigger => {
            clear_deferred_buffer_dirty(deferred, debounce_state);
            ResolvedBufferDirtyAction::Trigger
        }
        BufferDirtyAction::Debounce => {
            clear_deferred_buffer_dirty(deferred, debounce_state);
            debounce_state.notify_generation(buffer_generation);
            ResolvedBufferDirtyAction::NotifyDebounce
        }
        BufferDirtyAction::DeferUntilDisplay(action) => {
            *deferred = Some(DeferredBufferDirty {
                action,
                display_epoch: buffer_epoch,
                generation: buffer_generation,
            });
            debounce_state.defer_generation(buffer_generation);
            ResolvedBufferDirtyAction::None
        }
        BufferDirtyAction::Ignore => {
            clear_deferred_buffer_dirty(deferred, debounce_state);
            ResolvedBufferDirtyAction::None
        }
    }
}

fn clear_deferred_buffer_dirty(
    deferred: &mut Option<DeferredBufferDirty>,
    debounce_state: &mut DebounceState,
) {
    if let Some(pending) = deferred.take() {
        debounce_state.clear_deferred_generation(pending.generation);
    }
}

fn defer_cwd_trigger_until_buffer_display(
    deferred: &mut Option<DeferredBufferDirty>,
    debounce_state: &mut DebounceState,
    latest_buffer_epoch: Option<u64>,
    latest_buffer_generation: Option<u64>,
    display_epoch: u64,
) -> bool {
    if let Some(pending) = deferred.as_mut() {
        pending.action = DeferredBufferDirtyAction::TriggerNow;
        return true;
    }

    let (Some(buffer_epoch), Some(buffer_generation)) =
        (latest_buffer_epoch, latest_buffer_generation)
    else {
        return false;
    };
    if display_epoch > buffer_epoch {
        return false;
    }

    *deferred = Some(DeferredBufferDirty {
        action: DeferredBufferDirtyAction::TriggerNow,
        display_epoch: buffer_epoch,
        generation: buffer_generation,
    });
    debounce_state.defer_generation(buffer_generation);
    true
}

fn resolve_deferred_buffer_dirty(
    deferred: &mut Option<DeferredBufferDirty>,
    debounce_state: &mut DebounceState,
    display_epoch: u64,
    auto_trigger_enabled: bool,
    debounce_suppressed: bool,
) -> ResolvedBufferDirtyAction {
    let Some(pending) = *deferred else {
        return ResolvedBufferDirtyAction::None;
    };
    if display_epoch <= pending.display_epoch {
        return ResolvedBufferDirtyAction::None;
    }

    *deferred = None;
    let generation_is_current = debounce_state.current_generation == pending.generation;
    debounce_state.clear_deferred_generation(pending.generation);
    if !generation_is_current {
        return ResolvedBufferDirtyAction::None;
    }
    match pending.action {
        DeferredBufferDirtyAction::TriggerNow => {
            if auto_trigger_enabled {
                ResolvedBufferDirtyAction::Trigger
            } else {
                ResolvedBufferDirtyAction::None
            }
        }
        DeferredBufferDirtyAction::NotifyDebounce => {
            if auto_trigger_enabled && !debounce_suppressed {
                debounce_state.notify_generation(pending.generation);
                ResolvedBufferDirtyAction::NotifyDebounce
            } else {
                ResolvedBufferDirtyAction::None
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverlayWriteOutcome {
    Empty,
    Written,
    Stale,
}

fn write_pty_or_shutdown(
    pty_writer: &mut dyn Write,
    bytes: &[u8],
    operation: &'static str,
) -> std::io::Result<()> {
    pty_writer
        .write_all(bytes)
        .and_then(|()| pty_writer.flush())
        .map_err(|e| {
            tracing::debug!(operation, "PTY write/flush failed: {e}");
            e
        })
}

fn write_overlay_if_current(
    handler: &Arc<Mutex<InputHandler>>,
    ticket: OverlayWriteTicket,
    render_buf: &[u8],
) -> std::io::Result<OverlayWriteOutcome> {
    if render_buf.is_empty() {
        return Ok(OverlayWriteOutcome::Empty);
    }

    let mut h = match handler.lock() {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("overlay write skipped (handler lock poisoned): {e}");
            return Err(std::io::Error::other(format!(
                "handler lock poisoned during overlay write: {e}"
            )));
        }
    };
    if h.output_epoch() != ticket.epoch {
        tracing::trace!(
            render_epoch = ticket.epoch,
            current_epoch = h.output_epoch(),
            "dropping stale overlay render"
        );
        h.discard_overlay_ownership_after_stale_write(ticket);
        return Ok(OverlayWriteOutcome::Stale);
    }

    let mut stdout = std::io::stdout().lock();
    let write_result = stdout.write_all(render_buf).and_then(|()| stdout.flush());

    match write_result {
        Ok(()) => {
            h.commit_overlay_write(ticket);
            drop(h);
            Ok(OverlayWriteOutcome::Written)
        }
        Err(e) => {
            drop(h);
            tracing::debug!("overlay stdout write/flush failed: {e}");
            Err(e)
        }
    }
}

/// Debounce loop: waits for buffer-change notifications, resets a timer on each
/// new notification, and fires suggestions once the timer expires (typing pause).
async fn debounce_loop(
    notify: Arc<Notify>,
    debounce_state: Arc<Mutex<DebounceState>>,
    trigger_snapshot_gate: Arc<Mutex<()>>,
    handler: Arc<Mutex<InputHandler>>,
    parser: Arc<Mutex<TerminalParser>>,
    delay_ms: u64,
) {
    let delay = std::time::Duration::from_millis(delay_ms);
    loop {
        // Wait for first buffer change notification
        notify.notified().await;
        let mut generation = {
            let state = match debounce_state.lock() {
                Ok(state) => state,
                Err(e) => {
                    tracing::warn!("debounce skipped (state lock poisoned): {e}");
                    continue;
                }
            };
            match state.latest_notified_generation() {
                Some(generation) => generation,
                None => continue,
            }
        };

        // Debounce: reset timer on every new notification
        loop {
            tokio::select! {
                _ = notify.notified() => {
                    let state = match debounce_state.lock() {
                        Ok(state) => state,
                        Err(e) => {
                            tracing::warn!("debounce skipped (state lock poisoned): {e}");
                            break;
                        }
                    };
                    if let Some(next_generation) = state.latest_notified_generation() {
                        generation = next_generation;
                    }
                    continue;
                }
                _ = tokio::time::sleep(delay) => { break; }
            }
        }
        // Timer expired — fire trigger with bounded-block support.
        //
        // Phase 1: run suggest_sync under the handler lock and paint sync-only
        // results. If a high-priority async generator is pending and
        // render_block_ms > 0, we get back a `NeedsBlock` variant carrying
        // the channel receiver and sync geometry.
        let mut render_buf = Vec::new();
        let (prepared, render_ticket) = {
            let _snapshot_guard = match trigger_snapshot_gate.lock() {
                Ok(guard) => guard,
                Err(e) => {
                    tracing::warn!("debounce skipped (trigger snapshot gate poisoned): {e}");
                    continue;
                }
            };
            let fire_decision = {
                let state = match debounce_state.lock() {
                    Ok(state) => state,
                    Err(e) => {
                        tracing::warn!("debounce skipped (state lock poisoned): {e}");
                        continue;
                    }
                };
                debounce_fire_decision(&state, generation)
            };
            if fire_decision == DebounceFireDecision::Skip {
                continue;
            }
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
            let prepared = h.prepare_trigger_with_block(&parser, &mut render_buf);
            (prepared, h.overlay_write_ticket())
        };
        if !render_buf.is_empty() {
            if let Err(e) = write_overlay_if_current(&handler, render_ticket, &render_buf) {
                tracing::debug!("debounce overlay write/flush failed: {e}");
                break;
            }
        }

        // Phase 2 (only when blocking): await the generator outside the lock,
        // then re-acquire the lock to merge and repaint.
        if let TriggerPrepared::NeedsBlock {
            mut rx,
            sync_suggestions,
            block_ms,
            cursor_row,
            cursor_col,
            screen_rows,
            screen_cols,
            fingerprint,
            current_word,
        } = prepared
        {
            let timeout_dur = Duration::from_millis(block_ms);
            // Three-way race:
            // 1. Generator completes within the block window → merge + single paint.
            // 2. Timeout fires → restore rx, paint sync-only, dynamic_merge_loop
            //    delivers result when generator finishes later.
            // 3. New keystroke arrives (debounce notify) → abort the wait entirely.
            //    The outer debounce loop will re-fire trigger for the new buffer
            //    and overwrite `dynamic_rx` anyway, so we simply drop rx here.
            let (maybe_async, rx_after_recv, rx_on_timeout) = tokio::select! {
                maybe_result = rx.recv() => {
                    // Generator completed within the window (or sent empty).
                    (maybe_result, Some(rx), None)
                }
                _ = tokio::time::sleep(timeout_dur) => {
                    // Timeout: restore rx so dynamic_merge_loop can merge later.
                    (None, None, Some(rx))
                }
                _ = notify.notified() => {
                    // Keystroke supersedes. Abort the orphaned generator
                    // task (its results would land in a None rx and be
                    // silently discarded), then re-arm the notify so the
                    // outer loop re-fires immediately against the fresh
                    // buffer instead of waiting for the next keystroke.
                    drop(rx);
                    match handler.lock() {
                        Ok(mut h) => h.abort_dynamic_task_and_clear_ctx(),
                        Err(e) => tracing::warn!(
                            "handler mutex poisoned during keystroke-cancel cleanup: {e}"
                        ),
                    }
                    notify.notify_one();
                    continue;
                }
            };

            let mut render_buf2 = Vec::new();
            let render_ticket2 = {
                let _snapshot_guard = match trigger_snapshot_gate.lock() {
                    Ok(guard) => guard,
                    Err(e) => {
                        tracing::warn!(
                            "debounce apply_block_result skipped (trigger snapshot gate poisoned): {e}"
                        );
                        continue;
                    }
                };
                let continuation_decision = {
                    let state = match debounce_state.lock() {
                        Ok(state) => state,
                        Err(e) => {
                            tracing::warn!(
                                "debounce apply_block_result skipped (state lock poisoned): {e}"
                            );
                            continue;
                        }
                    };
                    debounce_continuation_decision(&state, generation)
                };
                if continuation_decision == DebounceContinuationDecision::Drop {
                    match handler.lock() {
                        Ok(mut h) => h.abort_dynamic_task_and_clear_ctx(),
                        Err(e) => tracing::warn!(
                            "handler mutex poisoned during stale block-result cleanup: {e}"
                        ),
                    }
                    continue;
                }
                let mut h = match handler.lock() {
                    Ok(h) => h,
                    Err(e) => {
                        tracing::warn!(
                            "debounce apply_block_result skipped (handler lock poisoned): {e}"
                        );
                        continue;
                    }
                };
                h.apply_block_result(
                    &parser,
                    &mut render_buf2,
                    maybe_async,
                    rx_after_recv,
                    rx_on_timeout,
                    sync_suggestions,
                    cursor_row,
                    cursor_col,
                    screen_rows,
                    screen_cols,
                    fingerprint,
                    &current_word,
                );
                h.overlay_write_ticket()
            };
            if !render_buf2.is_empty() {
                if let Err(e) = write_overlay_if_current(&handler, render_ticket2, &render_buf2) {
                    tracing::debug!("debounce overlay write/flush failed: {e}");
                    break;
                }
            }
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
    debounce_state: Arc<Mutex<DebounceState>>,
) {
    loop {
        notify.notified().await;
        loop {
            match locked_async_overlay_gate_decision(&debounce_state, "dynamic merge loop") {
                Some(AsyncOverlayGateDecision::Render) => break,
                Some(AsyncOverlayGateDecision::Blocked) => {
                    tokio::time::sleep(ASYNC_OVERLAY_GATE_RETRY).await;
                }
                None => return,
            }
        }
        let mut render_buf = Vec::new();
        let render_ticket = {
            let mut h = match handler.lock() {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!("dynamic merge skipped (handler lock poisoned): {e}");
                    continue;
                }
            };
            h.try_merge_dynamic(&parser, &mut render_buf);
            h.overlay_write_ticket()
        };
        if !render_buf.is_empty() {
            if let Err(e) = write_overlay_if_current(&handler, render_ticket, &render_buf) {
                tracing::debug!("dynamic merge overlay write/flush failed: {e}");
                break;
            }
        }
    }
}

async fn feedback_tick_loop(
    notify: Arc<Notify>,
    handler: Arc<Mutex<InputHandler>>,
    debounce_state: Arc<Mutex<DebounceState>>,
) {
    loop {
        notify.notified().await;
        let mut next_ms: u64 = 0;
        loop {
            if next_ms > 0 {
                tokio::time::sleep(Duration::from_millis(next_ms)).await;
            }
            match locked_async_overlay_gate_decision(&debounce_state, "feedback tick loop") {
                Some(AsyncOverlayGateDecision::Render) => {}
                Some(AsyncOverlayGateDecision::Blocked) => {
                    tokio::time::sleep(ASYNC_OVERLAY_GATE_RETRY).await;
                    continue;
                }
                None => return,
            }
            let mut render_buf: Vec<u8> = Vec::new();
            let (keep_running, render_ticket) = {
                let mut h = match handler.lock() {
                    Ok(h) => h,
                    Err(e) => {
                        tracing::warn!("feedback tick skipped (handler lock poisoned): {e}");
                        break;
                    }
                };
                let keep_running = if h.feedback_kind().is_loading() {
                    h.render_indicator_only(&mut render_buf);
                    next_ms = 80;
                    true
                } else if h.clear_expired_feedback(&mut render_buf) {
                    next_ms = 200;
                    false
                } else {
                    next_ms = 200;
                    h.feedback_kind().since().is_some()
                };
                (keep_running, h.overlay_write_ticket())
            };
            if !render_buf.is_empty() {
                match write_overlay_if_current(&handler, render_ticket, &render_buf) {
                    Ok(OverlayWriteOutcome::Written | OverlayWriteOutcome::Empty) => {}
                    Ok(OverlayWriteOutcome::Stale) => break,
                    Err(e) => {
                        tracing::debug!("feedback overlay write/flush failed: {e}");
                        break;
                    }
                }
            }
            if !keep_running {
                break;
            }
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

/// Outcome of dispatching a CPR response back through the proxy. Pure
/// transformation over `TerminalState` — extracted from Task A so the
/// FIFO ordering invariant can be unit-tested without spawning the
/// full proxy. Both `ForwardToPty` and `DropEmpty` carry the
/// coordinates so the caller can re-encode and write to the PTY; the
/// only difference is whether the empty-queue case warrants a warn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CprAction {
    SyncOurs(u16, u16),
    ForwardToPty(u16, u16),
    DropEmpty(u16, u16),
}

fn dispatch_cpr_response(state: &mut gc_parser::TerminalState, row: u16, col: u16) -> CprAction {
    match state.claim_next_cpr() {
        Some(CprOwner::Ours) => CprAction::SyncOurs(row, col),
        Some(CprOwner::Shell) => CprAction::ForwardToPty(row, col),
        None => CprAction::DropEmpty(row, col),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::KeyEvent;
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

    #[test]
    fn buffer_dirty_action_triggers_immediately_when_delay_zero() {
        assert_eq!(
            buffer_dirty_action(false, 0, true, false, true),
            BufferDirtyAction::Trigger
        );
    }

    #[test]
    fn buffer_dirty_action_defers_delay_zero_until_redraw() {
        assert_eq!(
            buffer_dirty_action(false, 0, true, false, false),
            BufferDirtyAction::DeferUntilDisplay(DeferredBufferDirtyAction::TriggerNow)
        );
    }

    #[test]
    fn buffer_dirty_action_ignores_when_auto_trigger_disabled() {
        assert_eq!(
            buffer_dirty_action(false, 0, false, false, true),
            BufferDirtyAction::Ignore
        );
    }

    #[test]
    fn buffer_dirty_action_ignores_when_debounce_suppressed() {
        assert_eq!(
            buffer_dirty_action(false, 0, true, true, true),
            BufferDirtyAction::Ignore
        );
    }

    #[test]
    fn buffer_dirty_action_defers_debounce_until_redraw_when_delay_positive() {
        assert_eq!(
            buffer_dirty_action(false, 150, true, false, false),
            BufferDirtyAction::DeferUntilDisplay(DeferredBufferDirtyAction::NotifyDebounce)
        );
    }

    #[test]
    fn buffer_dirty_action_debounces_after_redraw_when_delay_positive() {
        assert_eq!(
            buffer_dirty_action(false, 150, true, false, true),
            BufferDirtyAction::Debounce
        );
    }

    #[test]
    fn buffer_dirty_action_defers_pending_trigger_until_redraw() {
        assert_eq!(
            buffer_dirty_action(true, 150, true, true, false),
            BufferDirtyAction::DeferUntilDisplay(DeferredBufferDirtyAction::TriggerNow),
            "OSC-only buffer reports must not render before a later display epoch advances after the report"
        );
    }

    #[test]
    fn buffer_dirty_action_triggers_pending_after_redraw() {
        assert_eq!(
            buffer_dirty_action(true, 150, true, true, true),
            BufferDirtyAction::Trigger
        );
    }

    #[test]
    fn deferred_trigger_clears_when_suppressed_buffer_dirty_arrives() {
        let mut deferred = Some(DeferredBufferDirty {
            action: DeferredBufferDirtyAction::TriggerNow,
            display_epoch: 3,
            generation: 1,
        });
        let mut debounce_state = DebounceState {
            current_generation: 1,
            notified_generation: None,
            deferred_generation: Some(1),
        };

        let action = buffer_dirty_action(false, 150, true, true, false);
        assert_eq!(
            apply_buffer_dirty_action(&mut deferred, &mut debounce_state, 3, 1, action),
            ResolvedBufferDirtyAction::None
        );
        assert_eq!(deferred, None);
        assert_eq!(
            resolve_deferred_buffer_dirty(&mut deferred, &mut debounce_state, 4, true, true),
            ResolvedBufferDirtyAction::None
        );
    }

    #[test]
    fn deferred_debounce_notifies_only_after_later_display() {
        let mut deferred = None;
        let mut debounce_state = DebounceState::default();
        let generation = debounce_state.next_buffer_generation();
        let action = buffer_dirty_action(false, 150, true, false, false);

        assert_eq!(
            apply_buffer_dirty_action(&mut deferred, &mut debounce_state, 8, generation, action),
            ResolvedBufferDirtyAction::None
        );
        assert_eq!(
            deferred,
            Some(DeferredBufferDirty {
                action: DeferredBufferDirtyAction::NotifyDebounce,
                display_epoch: 8,
                generation,
            })
        );
        assert_eq!(
            resolve_deferred_buffer_dirty(&mut deferred, &mut debounce_state, 8, true, false),
            ResolvedBufferDirtyAction::None
        );
        assert_eq!(
            resolve_deferred_buffer_dirty(&mut deferred, &mut debounce_state, 9, true, false),
            ResolvedBufferDirtyAction::NotifyDebounce
        );
        assert_eq!(deferred, None);
        assert!(debounce_state.can_fire_generation(generation));
    }

    #[test]
    fn deferred_debounce_generation_blocks_prior_timer_until_redraw() {
        let mut deferred = None;
        let mut debounce_state = DebounceState::default();

        let first_generation = debounce_state.next_buffer_generation();
        assert_eq!(
            apply_buffer_dirty_action(
                &mut deferred,
                &mut debounce_state,
                3,
                first_generation,
                BufferDirtyAction::Debounce,
            ),
            ResolvedBufferDirtyAction::NotifyDebounce
        );
        assert!(debounce_state.can_fire_generation(first_generation));

        let second_generation = debounce_state.next_buffer_generation();
        assert_eq!(
            apply_buffer_dirty_action(
                &mut deferred,
                &mut debounce_state,
                4,
                second_generation,
                BufferDirtyAction::DeferUntilDisplay(DeferredBufferDirtyAction::NotifyDebounce),
            ),
            ResolvedBufferDirtyAction::None
        );
        assert!(!debounce_state.can_fire_generation(first_generation));
        assert!(!debounce_state.can_fire_generation(second_generation));

        assert_eq!(
            resolve_deferred_buffer_dirty(&mut deferred, &mut debounce_state, 4, true, false),
            ResolvedBufferDirtyAction::None
        );
        assert!(!debounce_state.can_fire_generation(first_generation));

        assert_eq!(
            resolve_deferred_buffer_dirty(&mut deferred, &mut debounce_state, 5, true, false),
            ResolvedBufferDirtyAction::NotifyDebounce
        );
        assert!(!debounce_state.can_fire_generation(first_generation));
        assert!(debounce_state.can_fire_generation(second_generation));
    }

    #[test]
    fn debounce_fire_decision_rejects_generation_invalidated_by_forwarded_input() {
        let mut debounce_state = DebounceState::default();
        let generation = debounce_state.next_buffer_generation();
        debounce_state.notify_generation(generation);
        assert_eq!(
            debounce_fire_decision(&debounce_state, generation),
            DebounceFireDecision::Fire
        );

        debounce_state.invalidate_for_forwarded_input();

        assert_eq!(
            debounce_fire_decision(&debounce_state, generation),
            DebounceFireDecision::Skip
        );
    }

    #[test]
    fn debounce_fire_decision_rejects_prior_generation_when_newer_report_is_deferred() {
        let mut deferred = None;
        let mut debounce_state = DebounceState::default();
        let first_generation = debounce_state.next_buffer_generation();
        debounce_state.notify_generation(first_generation);

        let second_generation = debounce_state.next_buffer_generation();
        assert_eq!(
            apply_buffer_dirty_action(
                &mut deferred,
                &mut debounce_state,
                4,
                second_generation,
                BufferDirtyAction::DeferUntilDisplay(DeferredBufferDirtyAction::NotifyDebounce),
            ),
            ResolvedBufferDirtyAction::None
        );

        assert_eq!(
            debounce_fire_decision(&debounce_state, first_generation),
            DebounceFireDecision::Skip
        );
        assert_eq!(
            debounce_continuation_decision(&debounce_state, first_generation),
            DebounceContinuationDecision::Drop
        );
    }

    #[test]
    fn async_overlay_gate_blocks_while_redraw_deferred() {
        let mut debounce_state = DebounceState::default();
        let generation = debounce_state.next_buffer_generation();
        debounce_state.defer_generation(generation);

        assert_eq!(
            async_overlay_gate_decision(&debounce_state),
            AsyncOverlayGateDecision::Blocked
        );

        debounce_state.clear_deferred_generation(generation);

        assert_eq!(
            async_overlay_gate_decision(&debounce_state),
            AsyncOverlayGateDecision::Render
        );
    }

    #[test]
    fn deferred_trigger_survives_forwarded_input_that_advances_display() {
        let mut deferred = None;
        let mut debounce_state = DebounceState::default();
        let generation = debounce_state.next_buffer_generation();
        assert_eq!(
            apply_buffer_dirty_action(
                &mut deferred,
                &mut debounce_state,
                4,
                generation,
                BufferDirtyAction::DeferUntilDisplay(DeferredBufferDirtyAction::TriggerNow),
            ),
            ResolvedBufferDirtyAction::None
        );

        debounce_state.invalidate_for_forwarded_input();

        assert_eq!(
            resolve_deferred_buffer_dirty(&mut deferred, &mut debounce_state, 5, true, false),
            ResolvedBufferDirtyAction::Trigger
        );
        assert_eq!(deferred, None);
    }

    #[test]
    fn deferred_debounce_survives_forwarded_input_that_advances_display() {
        let mut deferred = None;
        let mut debounce_state = DebounceState::default();
        let generation = debounce_state.next_buffer_generation();
        assert_eq!(
            apply_buffer_dirty_action(
                &mut deferred,
                &mut debounce_state,
                4,
                generation,
                BufferDirtyAction::DeferUntilDisplay(DeferredBufferDirtyAction::NotifyDebounce),
            ),
            ResolvedBufferDirtyAction::None
        );

        debounce_state.invalidate_for_forwarded_input();

        assert_eq!(
            resolve_deferred_buffer_dirty(&mut deferred, &mut debounce_state, 5, true, false),
            ResolvedBufferDirtyAction::NotifyDebounce
        );
        assert_eq!(deferred, None);
        assert!(debounce_state.can_fire_generation(generation));
    }

    #[test]
    fn pty_read_records_buffer_generation_when_report_updates_parser() {
        let mut parser = TerminalParser::new(24, 80);
        let mut display_epoch = 0;
        let mut next_generation = 0;
        let events = process_pty_read(
            &mut parser,
            b"\x1b]7770;4;git \x07",
            &mut display_epoch,
            || {
                next_generation += 1;
                next_generation
            },
        );

        assert_eq!(events.latest_buffer_epoch, Some(display_epoch));
        assert_eq!(events.latest_buffer_generation, Some(1));
        assert_eq!(next_generation, 1);
        assert!(!parser.state_mut().take_buffer_dirty());
    }

    #[test]
    fn cwd_dirty_defers_after_ignored_undisplayed_buffer_report() {
        let mut deferred = None;
        let mut debounce_state = DebounceState::default();
        let prior_generation = debounce_state.next_buffer_generation();
        debounce_state.notify_generation(prior_generation);
        assert!(debounce_state.can_fire_generation(prior_generation));

        let current_generation = debounce_state.next_buffer_generation();
        assert_eq!(
            apply_buffer_dirty_action(
                &mut deferred,
                &mut debounce_state,
                4,
                current_generation,
                BufferDirtyAction::Ignore,
            ),
            ResolvedBufferDirtyAction::None
        );
        assert_eq!(deferred, None);

        assert!(defer_cwd_trigger_until_buffer_display(
            &mut deferred,
            &mut debounce_state,
            Some(4),
            Some(current_generation),
            4,
        ));
        assert_eq!(
            deferred,
            Some(DeferredBufferDirty {
                action: DeferredBufferDirtyAction::TriggerNow,
                display_epoch: 4,
                generation: current_generation,
            })
        );
        assert!(!debounce_state.can_fire_generation(prior_generation));

        assert_eq!(
            resolve_deferred_buffer_dirty(&mut deferred, &mut debounce_state, 4, true, false),
            ResolvedBufferDirtyAction::None
        );
        assert_eq!(
            resolve_deferred_buffer_dirty(&mut deferred, &mut debounce_state, 5, true, false),
            ResolvedBufferDirtyAction::Trigger
        );
    }

    #[test]
    fn cwd_dirty_upgrades_pending_deferred_buffer_to_trigger_after_redraw() {
        let generation = 2;
        let mut deferred = Some(DeferredBufferDirty {
            action: DeferredBufferDirtyAction::NotifyDebounce,
            display_epoch: 4,
            generation,
        });
        let mut debounce_state = DebounceState {
            current_generation: generation,
            notified_generation: Some(1),
            deferred_generation: Some(generation),
        };

        assert!(defer_cwd_trigger_until_buffer_display(
            &mut deferred,
            &mut debounce_state,
            None,
            None,
            4,
        ));
        assert_eq!(
            deferred.map(|pending| pending.action),
            Some(DeferredBufferDirtyAction::TriggerNow)
        );
        assert!(!debounce_state.can_fire_generation(1));
        assert_eq!(
            resolve_deferred_buffer_dirty(&mut deferred, &mut debounce_state, 5, true, false),
            ResolvedBufferDirtyAction::Trigger
        );
        assert_eq!(deferred, None);
        assert!(!debounce_state.can_fire_generation(1));
    }

    #[test]
    fn deferred_debounce_is_dropped_if_suppression_changes_before_redraw() {
        let mut deferred = Some(DeferredBufferDirty {
            action: DeferredBufferDirtyAction::NotifyDebounce,
            display_epoch: 4,
            generation: 1,
        });
        let mut debounce_state = DebounceState {
            current_generation: 1,
            notified_generation: None,
            deferred_generation: Some(1),
        };

        assert_eq!(
            resolve_deferred_buffer_dirty(&mut deferred, &mut debounce_state, 5, true, true),
            ResolvedBufferDirtyAction::None
        );
        assert_eq!(deferred, None);
    }

    #[test]
    fn current_buffer_dirty_trigger_replaces_deferred_without_double_fire() {
        let mut deferred = Some(DeferredBufferDirty {
            action: DeferredBufferDirtyAction::TriggerNow,
            display_epoch: 1,
            generation: 1,
        });
        let mut debounce_state = DebounceState {
            current_generation: 2,
            notified_generation: None,
            deferred_generation: Some(1),
        };
        let action = buffer_dirty_action(true, 150, true, false, true);

        assert_eq!(
            apply_buffer_dirty_action(&mut deferred, &mut debounce_state, 2, 2, action),
            ResolvedBufferDirtyAction::Trigger
        );
        assert_eq!(deferred, None);
        assert_eq!(
            resolve_deferred_buffer_dirty(&mut deferred, &mut debounce_state, 2, true, false),
            ResolvedBufferDirtyAction::None
        );
    }

    #[test]
    fn pty_read_tracks_display_epoch_before_latest_buffer_report() {
        let mut parser = TerminalParser::new(24, 80);
        let mut display_epoch = 0;
        let events = process_pty_read(
            &mut parser,
            b"x\x1b]7770;4;git \x07",
            &mut display_epoch,
            || 1,
        );

        assert!(events.display_dirty);
        assert_eq!(events.latest_buffer_epoch, Some(display_epoch));
        assert!(
            display_epoch <= events.latest_buffer_epoch.unwrap(),
            "display bytes before the latest OSC report must not satisfy that report"
        );
    }

    #[test]
    fn pty_read_tracks_display_epoch_after_latest_buffer_report() {
        let mut parser = TerminalParser::new(24, 80);
        let mut display_epoch = 0;
        let events = process_pty_read(
            &mut parser,
            b"\x1b]7770;4;git \x07x",
            &mut display_epoch,
            || 1,
        );

        assert!(events.display_dirty);
        assert!(display_epoch > events.latest_buffer_epoch.unwrap());
    }

    #[test]
    fn pty_read_uses_latest_buffer_epoch_when_reports_coalesce() {
        let mut parser = TerminalParser::new(24, 80);
        let mut display_epoch = 0;
        let mut next_generation = 0;
        let events = process_pty_read(
            &mut parser,
            b"\x1b]7770;4;git \x07x\x1b]7770;5;git s\x07",
            &mut display_epoch,
            || {
                next_generation += 1;
                next_generation
            },
        );

        assert!(events.display_dirty);
        assert_eq!(display_epoch, 1);
        assert_eq!(events.latest_buffer_epoch, Some(1));
        assert_eq!(events.latest_buffer_generation, Some(2));
        assert_eq!(next_generation, 2);
        assert!(display_epoch <= events.latest_buffer_epoch.unwrap());
    }

    #[test]
    fn pty_read_allows_display_after_latest_coalesced_buffer_report() {
        let mut parser = TerminalParser::new(24, 80);
        let mut display_epoch = 0;
        let mut next_generation = 0;
        let events = process_pty_read(
            &mut parser,
            b"\x1b]7770;4;git \x07x\x1b]7770;5;git s\x07y",
            &mut display_epoch,
            || {
                next_generation += 1;
                next_generation
            },
        );

        assert!(events.display_dirty);
        assert_eq!(display_epoch, 2);
        assert_eq!(events.latest_buffer_epoch, Some(1));
        assert_eq!(events.latest_buffer_generation, Some(2));
        assert_eq!(next_generation, 2);
        assert!(display_epoch > events.latest_buffer_epoch.unwrap());
    }

    use gc_parser::TerminalParser;

    fn make_state(rows: u16, cols: u16) -> TerminalParser {
        TerminalParser::new(rows, cols)
    }

    fn parser_with_buffer(buffer: &str) -> Arc<Mutex<TerminalParser>> {
        let parser = Arc::new(Mutex::new(TerminalParser::new(24, 80)));
        let cursor = buffer.chars().count();
        let osc = format!("\x1b]7770;{cursor};{buffer}\x07");
        parser.lock().unwrap().process_bytes(osc.as_bytes());
        parser
    }

    #[test]
    fn dispatch_with_ours_at_head_syncs() {
        let mut p = make_state(24, 80);
        p.state_mut().enqueue_cpr(CprOwner::Ours);
        let action = dispatch_cpr_response(p.state_mut(), 5, 10);
        assert_eq!(action, CprAction::SyncOurs(5, 10));
    }

    #[test]
    fn dispatch_with_shell_at_head_forwards() {
        let mut p = make_state(24, 80);
        p.state_mut().enqueue_cpr(CprOwner::Shell);
        let action = dispatch_cpr_response(p.state_mut(), 3, 7);
        assert_eq!(action, CprAction::ForwardToPty(3, 7));
    }

    #[test]
    fn dispatch_with_empty_queue_returns_drop() {
        let mut p = make_state(24, 80);
        let action = dispatch_cpr_response(p.state_mut(), 1, 1);
        assert_eq!(action, CprAction::DropEmpty(1, 1));
    }

    #[test]
    fn deferred_sync_reschedules_when_shell_cpr_in_flight() {
        // Push Shell first (e.g., the shell sent CSI 6n), then Ours (proxy
        // queued its own request next). Responses must dispatch in that
        // same send-order — never the reverse. This is the bug class the
        // FIFO ordering fixes.
        let mut p = make_state(24, 80);
        p.state_mut().enqueue_cpr(CprOwner::Shell);
        p.state_mut().enqueue_cpr(CprOwner::Ours);
        assert_eq!(
            dispatch_cpr_response(p.state_mut(), 1, 1),
            CprAction::ForwardToPty(1, 1)
        );
        assert_eq!(
            dispatch_cpr_response(p.state_mut(), 2, 2),
            CprAction::SyncOurs(2, 2)
        );
    }

    #[test]
    fn shell_cpr_arrives_while_our_cpr_pending() {
        // Reverse order: proxy queued Ours first, then a shell program
        // sent CSI 6n. Responses must dispatch in that same order.
        let mut p = make_state(24, 80);
        p.state_mut().enqueue_cpr(CprOwner::Ours);
        p.state_mut().enqueue_cpr(CprOwner::Shell);
        assert_eq!(
            dispatch_cpr_response(p.state_mut(), 4, 4),
            CprAction::SyncOurs(4, 4)
        );
        assert_eq!(
            dispatch_cpr_response(p.state_mut(), 5, 5),
            CprAction::ForwardToPty(5, 5)
        );
    }

    #[test]
    fn rollback_ours_after_shell_preserves_shell_dispatch() {
        // Task B enqueues Ours on top of an already-pending Shell entry,
        // then the stdout write fails before `CSI 6n` reached the terminal.
        // Rolling back the Ours token must leave the queue with just the
        // Shell entry, and the next CPR response must still dispatch to
        // ForwardToPty with no Ours residue.
        let mut p = make_state(24, 80);
        p.state_mut().enqueue_cpr(CprOwner::Shell);
        let ours = p.state_mut().enqueue_cpr(CprOwner::Ours);
        assert!(p.state_mut().rollback_cpr(ours));
        assert_eq!(p.state().cpr_queue_len(), 1);
        assert_eq!(
            dispatch_cpr_response(p.state_mut(), 7, 3),
            CprAction::ForwardToPty(7, 3)
        );
        assert_eq!(p.state().cpr_queue_len(), 0);
    }

    #[test]
    fn write_overlay_if_current_drops_stale_overlay_after_epoch_bump() {
        let handler = Arc::new(Mutex::new(
            InputHandler::new(&[], gc_terminal::TerminalProfile::for_ghostty()).expect("handler"),
        ));
        let stale_ticket = handler.lock().expect("handler").overlay_write_ticket();

        {
            let mut h = handler.lock().expect("handler");
            h.handle_terminal_output(&mut Vec::new(), false, 1);
            assert_ne!(h.output_epoch(), stale_ticket.epoch);
        }

        let outcome = write_overlay_if_current(&handler, stale_ticket, b"stale overlay bytes")
            .expect("stale overlay should not be an I/O error");

        assert_eq!(outcome, OverlayWriteOutcome::Stale);
    }

    #[test]
    fn write_overlay_if_current_discards_owned_state_on_stale_write() {
        let handler = Arc::new(Mutex::new(
            InputHandler::new(&[], gc_terminal::TerminalProfile::for_ghostty()).expect("handler"),
        ));
        let parser = parser_with_buffer("git ");
        let (stale_ticket, stale_buf) = {
            let mut h = handler.lock().expect("handler");
            let mut render_buf = Vec::new();
            h.trigger(&parser, &mut render_buf);
            assert!(!render_buf.is_empty(), "setup: trigger should render popup");
            (h.overlay_write_ticket(), render_buf)
        };

        {
            let mut h = handler.lock().expect("handler");
            h.handle_terminal_output(&mut Vec::new(), false, 1);
            assert_ne!(h.output_epoch(), stale_ticket.epoch);
        }

        let outcome = write_overlay_if_current(&handler, stale_ticket, &stale_buf)
            .expect("stale overlay should not be an I/O error");

        assert_eq!(outcome, OverlayWriteOutcome::Stale);
        assert!(
            !handler.lock().expect("handler").has_overlay_ownership(),
            "handler must not keep ownership for overlay bytes that never reached stdout"
        );
    }

    #[test]
    fn write_overlay_if_current_preserves_newer_overlay_after_stale_render_race() {
        let handler = Arc::new(Mutex::new(
            InputHandler::new(&[], gc_terminal::TerminalProfile::for_ghostty()).expect("handler"),
        ));
        let parser = parser_with_buffer("git ");
        let (stale_ticket, stale_buf) = {
            let mut h = handler.lock().expect("handler");
            let mut render_buf = Vec::new();
            h.trigger(&parser, &mut render_buf);
            assert!(
                !render_buf.is_empty(),
                "setup: first render should produce bytes"
            );
            (h.overlay_write_ticket(), render_buf)
        };

        {
            let mut h = handler.lock().expect("handler");
            let mut newer_buf = Vec::new();
            h.process_key(&KeyEvent::ArrowDown, &parser, &mut newer_buf);
            assert!(
                !newer_buf.is_empty(),
                "setup: newer repaint should produce bytes"
            );
            assert_ne!(h.output_epoch(), stale_ticket.epoch);
            assert!(
                h.has_overlay_ownership(),
                "setup: newer overlay ownership should still be current"
            );
        }

        let outcome = write_overlay_if_current(&handler, stale_ticket, &stale_buf)
            .expect("stale overlay should not be an I/O error");

        assert_eq!(outcome, OverlayWriteOutcome::Stale);
        assert!(
            handler.lock().expect("handler").has_overlay_ownership(),
            "dropping an older stale render must not clear newer overlay ownership"
        );
    }

    #[test]
    fn write_overlay_if_current_lets_shell_cleanup_supersede_stale_teardown_cleanup() {
        let handler = Arc::new(Mutex::new(
            InputHandler::new(&[], gc_terminal::TerminalProfile::for_ghostty()).expect("handler"),
        ));
        let parser = parser_with_buffer("git ");

        {
            let mut h = handler.lock().expect("handler");
            let mut initial_buf = Vec::new();
            h.trigger(&parser, &mut initial_buf);
            assert!(
                !initial_buf.is_empty(),
                "setup: trigger should render popup"
            );
            let initial_ticket = h.overlay_write_ticket();
            h.commit_overlay_write(initial_ticket);
            assert!(
                h.has_overlay_ownership(),
                "setup: committed popup should own a layout"
            );
        }

        let (cleanup_ticket, cleanup_buf) = {
            let mut h = handler.lock().expect("handler");
            let mut cleanup_buf = Vec::new();
            let forward = h.process_key(&KeyEvent::Printable('x'), &parser, &mut cleanup_buf);
            assert_eq!(forward, b"x");
            assert!(
                !cleanup_buf.is_empty(),
                "setup: typing into a visible popup should stage cleanup bytes"
            );
            assert!(
                h.has_overlay_ownership(),
                "staged teardown cleanup must keep layout ownership until the cleanup is written"
            );
            (h.overlay_write_ticket(), cleanup_buf)
        };

        {
            let mut h = handler.lock().expect("handler");
            let mut shell_cleanup = Vec::new();
            h.handle_terminal_output(&mut shell_cleanup, true, 0);
            assert!(
                !shell_cleanup.is_empty(),
                "shell output racing the pending teardown must clear the still-owned layout"
            );
            assert_ne!(h.output_epoch(), cleanup_ticket.epoch);
        }

        let outcome = write_overlay_if_current(&handler, cleanup_ticket, &cleanup_buf)
            .expect("stale cleanup should not be an I/O error");

        assert_eq!(outcome, OverlayWriteOutcome::Stale);
        assert!(
            !handler.lock().expect("handler").has_overlay_ownership(),
            "shell-output cleanup should have superseded the stale teardown cleanup"
        );
    }

    struct SpawnedTestChild {
        child: Box<dyn portable_pty::Child + Send + Sync>,
        // Held so the slave side of the PTY stays open — dropping the master
        // elsewhere would SIGHUP the child and invalidate the exit code.
        _master: Box<dyn portable_pty::MasterPty + Send>,
    }

    fn spawn_child(argv: &[&str]) -> SpawnedTestChild {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new(argv[0]);
        for a in &argv[1..] {
            cmd.arg(a);
        }
        let child = pair.slave.spawn_command(cmd).expect("spawn_command");
        drop(pair.slave);
        SpawnedTestChild {
            child,
            _master: pair.master,
        }
    }

    #[test]
    fn wait_with_timeout_returns_before_deadline_for_live_child() {
        let mut spawned = spawn_child(&["sleep", "30"]);
        let pid_before = spawned.child.process_id();
        let start = std::time::Instant::now();
        let code = wait_with_timeout(spawned.child.as_mut(), Duration::from_millis(200));
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(1500),
            "wait_with_timeout must return promptly, took {elapsed:?}"
        );
        assert!(
            matches!(spawned.child.try_wait(), Ok(Some(_))),
            "child must be reaped (pid was {pid_before:?})"
        );
        // portable-pty maps signal-killed children to exit code 1 (since
        // `std::process::ExitStatus::code()` is `None` for signalled
        // termination). The bounded kill-then-reap path must not return 143.
        assert_ne!(
            code, 143,
            "live child must be reaped within bound, not reported as orphan"
        );
    }

    #[test]
    fn wait_with_timeout_kills_child_that_ignores_sigterm() {
        let mut spawned = spawn_child(&["sh", "-c", "trap \"\" TERM; sleep 30"]);
        let start = std::time::Instant::now();
        let code = wait_with_timeout(spawned.child.as_mut(), Duration::from_millis(200));
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(2000),
            "SIGTERM-ignoring child must still be reaped within bound, took {elapsed:?}"
        );
        assert!(
            matches!(spawned.child.try_wait(), Ok(Some(_))),
            "SIGTERM-ignoring child must have been SIGKILLed and reaped"
        );
        assert_ne!(
            code, 143,
            "SIGKILL path must reap the child, not leave it orphaned"
        );
    }

    #[test]
    fn wait_with_timeout_returns_exit_code_of_already_exited_child() {
        let mut spawned = spawn_child(&["sh", "-c", "exit 7"]);
        // Give the shell enough time to exit cleanly.
        std::thread::sleep(Duration::from_millis(200));
        let code = wait_with_timeout(spawned.child.as_mut(), Duration::from_millis(500));
        assert_eq!(
            code, 7,
            "already-exited child must return its real exit code"
        );
    }
}
