use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use gc_parser::{CprOwner, TerminalParser};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{mpsc, Notify};

use gc_config::GhostConfig;

use gc_overlay::{parse_style, PopupTheme};
use gc_suggest::spec_dirs::{resolve_spec_dirs_with_provenance, SpecDirResolution};
use gc_terminal::TerminalProfile;

use crate::config_watch::spawn_config_watcher;
use crate::handler::{InputHandler, Keybindings, OverlayWriteTicket, TriggerPrepared};
use crate::input::KeyParser;
use crate::resize::{get_terminal_size, resize_pty};
use crate::spawn::{spawn_shell, SpawnedShell};

/// Upper bound on how long a queued CPR entry may sit before we prune it.
/// A misbehaving terminal that silently drops `CSI 6n` would otherwise leak
/// queue entries forever. A late response after prune lands as
/// `CprAction::DropEmpty` and is forwarded defensively, which is why the
/// threshold is generous.
const CPR_STALE_THRESHOLD: Duration = Duration::from_secs(30);

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

fn input_handler_for_resolution(
    resolution: &SpecDirResolution,
    terminal_profile: TerminalProfile,
) -> Result<InputHandler> {
    match InputHandler::new_with_embedded(
        &resolution.dirs,
        terminal_profile.clone(),
        resolution.include_embedded,
    ) {
        Ok(handler) => Ok(handler),
        Err(e) => {
            tracing::warn!("failed to init suggestion engine: {}, trying fallback", e);
            InputHandler::new_with_embedded(
                &[std::path::PathBuf::from(".")],
                terminal_profile,
                true,
            )
            .context("fallback handler also failed — cannot start proxy")
        }
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

    // Resolve spec directories from config, preserving whether runtime should
    // supplement them with embedded specs.
    let spec_dir_resolution = resolve_spec_dirs_with_provenance(&config.paths.spec_dirs);

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
    let handler = Arc::new(Mutex::new(
        input_handler_for_resolution(&spec_dir_resolution, terminal_profile)?
            .with_keybindings(keybindings)
            .with_theme(theme)
            .with_popup_config(config.popup.max_visible)
            .with_popup_widths(config.popup.min_width, config.popup.max_width)
            .with_description_box(
                config.popup.description_box,
                config.popup.description_box_max_width,
                config.popup.description_box_lines,
                config.popup.description_box_debounce_ms,
            )
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
            ),
    ));

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
    let feedback_handle = tokio::spawn(async move {
        feedback_tick_loop(feedback_notify, handler_for_feedback).await;
    });

    // Detail-box debounce loop: re-renders the popup after the
    // description-box debounce window expires so the box catches up to a
    // settled selection.
    let detail_notify = {
        let h = match handler.lock() {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("handler mutex poisoned during detail-redraw setup: {e}");
                anyhow::bail!(
                    "handler mutex poisoned during detail-redraw setup — cannot start proxy"
                );
            }
        };
        h.detail_redraw_notify()
    };
    let handler_for_detail = Arc::clone(&handler);
    let parser_for_detail = Arc::clone(&parser);
    let detail_handle = tokio::spawn(async move {
        detail_redraw_loop(detail_notify, handler_for_detail, parser_for_detail).await;
    });

    // Background sweep task for the spec cache. Held in scope for the
    // duration of the proxy event loop; dropped when run_proxy returns,
    // which cancels the task.
    let _spec_cache_sweep = {
        let h = match handler.lock() {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("handler mutex poisoned during spec-cache sweep setup: {e}");
                anyhow::bail!(
                    "handler mutex poisoned during spec-cache sweep setup — cannot start proxy"
                );
            }
        };
        h.spawn_spec_cache_sweep(config.suggest.spec_cache.clone())
    };

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
                    let mut h = match handler_for_stdin.lock() {
                        Ok(h) => h,
                        Err(e) => {
                            tracing::warn!("handler mutex poisoned in stdin task: {e}");
                            break 'stdin;
                        }
                    };
                    let forward = h.process_key(key, &parser_for_stdin, &mut render_buf);
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
    let stdout_handle = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 8192];
        let mut pending_trigger = PendingTrigger::new();
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break, // PTY closed
                Ok(n) => n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };

            // Feed bytes through the VT parser to track terminal state
            let (needs_cpr, display_dirty, viewport_scrolls) = {
                let mut p = match parser_for_stdout.lock() {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!("parser mutex poisoned in stdout task: {e}");
                        break;
                    }
                };
                p.process_bytes(&buf[..n]);
                let state = p.state_mut();
                (
                    state.take_cursor_sync_requested(),
                    state.take_display_dirty(),
                    state.take_viewport_scroll_count(),
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

            let (buffer_dirty, buffer_pending_display) = {
                match parser_for_stdout.lock() {
                    Ok(mut p) => {
                        let state = p.state_mut();
                        (state.take_buffer_dirty(), state.buffer_pending_display())
                    }
                    Err(e) => {
                        tracing::warn!("parser mutex poisoned in stdout task: {e}");
                        break;
                    }
                }
            };

            if buffer_dirty {
                let mut render_buf = Vec::new();
                let render_ticket = {
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
                        buffer_pending_display,
                    );
                    if had_pending_trigger {
                        h.clear_trigger_request();
                    }
                    match action {
                        BufferDirtyAction::Immediate(Action::Trigger) => {
                            pending_trigger.clear_for_supersede("immediate Trigger");
                            h.trigger(&parser_for_stdout, &mut render_buf);
                        }
                        BufferDirtyAction::Immediate(Action::Debounce) => {
                            pending_trigger.clear_for_supersede("immediate Debounce");
                            debounce_notify_b.notify_one();
                        }
                        BufferDirtyAction::Defer(action) => {
                            pending_trigger.stash(action);
                        }
                        BufferDirtyAction::Ignore => {
                            pending_trigger.clear_for_supersede("Ignore");
                        }
                    }
                    h.overlay_write_ticket()
                };
                if !render_buf.is_empty() {
                    if let Err(e) =
                        write_overlay_if_current(&handler_for_stdout, render_ticket, &render_buf)
                    {
                        tracing::debug!("Task B overlay write/flush failed: {e}");
                        break;
                    }
                }
            }

            if pending_trigger.is_pending() && !buffer_pending_display {
                let mut render_buf = Vec::new();
                let render_ticket = {
                    let mut h = match handler_for_stdout.lock() {
                        Ok(h) => h,
                        Err(e) => {
                            tracing::warn!("handler mutex poisoned in stdout task: {e}");
                            break;
                        }
                    };
                    let resolved = pending_trigger
                        .resolve(h.auto_trigger_enabled(), h.is_debounce_suppressed());
                    match resolved {
                        Some(Action::Trigger) => {
                            h.trigger(&parser_for_stdout, &mut render_buf);
                        }
                        Some(Action::Debounce) => {
                            debounce_notify_b.notify_one();
                        }
                        None => {}
                    }
                    h.overlay_write_ticket()
                };
                if !render_buf.is_empty() {
                    if let Err(e) =
                        write_overlay_if_current(&handler_for_stdout, render_ticket, &render_buf)
                    {
                        tracing::debug!("Task B deferred overlay write/flush failed: {e}");
                        break;
                    }
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
                let render_ticket = {
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
                    h.overlay_write_ticket()
                };
                if !render_buf.is_empty() {
                    if let Err(e) =
                        write_overlay_if_current(&handler_for_stdout, render_ticket, &render_buf)
                    {
                        tracing::debug!("Task B overlay write/flush failed: {e}");
                        break;
                    }
                }
            }

            // Poll for dynamic (script generator) results — non-blocking.
            {
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
                    if let Err(e) =
                        write_overlay_if_current(&handler_for_stdout, render_ticket, &render_buf)
                    {
                        tracing::debug!("Task B overlay write/flush failed: {e}");
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
    detail_handle.abort();
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

/// What the dispatcher should do with a single suggestion-relevant action,
/// independent of timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Fire suggestions synchronously. Bypasses `debounce_suppressed` because
    /// the variant is only produced by paths that already cleared that gate —
    /// `has_pending_trigger=true` (returns Trigger before the suppression
    /// check) or `delay_ms == 0` (only reached when the suppression check
    /// returned false).
    Trigger,
    /// Notify the debounce loop; the loop re-checks suppression at fire time.
    Debounce,
}

/// Outcome of `buffer_dirty_action` — three-way split over the action axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BufferDirtyAction {
    /// Apply the action immediately; the display has caught up to the buffer report.
    Immediate(Action),
    /// Stash the action until the parser observes a display-changing byte;
    /// ensures popup geometry is computed against a current cursor.
    Defer(Action),
    /// Drop the buffer event (auto-trigger disabled, or debounce suppressed
    /// with no pending user-trigger to bypass it).
    Ignore,
}

fn buffer_dirty_action(
    has_pending_trigger: bool,
    delay_ms: u64,
    auto_trigger_enabled: bool,
    debounce_suppressed: bool,
    buffer_pending_display: bool,
) -> BufferDirtyAction {
    if !auto_trigger_enabled {
        return BufferDirtyAction::Ignore;
    }
    if has_pending_trigger {
        return if buffer_pending_display {
            BufferDirtyAction::Defer(Action::Trigger)
        } else {
            BufferDirtyAction::Immediate(Action::Trigger)
        };
    }
    if debounce_suppressed {
        return BufferDirtyAction::Ignore;
    }
    if delay_ms == 0 {
        if buffer_pending_display {
            BufferDirtyAction::Defer(Action::Trigger)
        } else {
            BufferDirtyAction::Immediate(Action::Trigger)
        }
    } else if buffer_pending_display {
        BufferDirtyAction::Defer(Action::Debounce)
    } else {
        BufferDirtyAction::Immediate(Action::Debounce)
    }
}

/// Single-slot stash for a deferred `Action`. Encapsulates the three
/// invariants that the stdout task relies on:
///
/// 1. `stash` unconditionally overwrites — a newer buffer event always
///    reflects newer cursor geometry, so merging or keeping the old entry
///    would let stale geometry win.
/// 2. `resolve` always drains the slot when present, even when gating drops
///    the action — leaving an entry behind would let it fire on a later
///    redraw cycle that no longer matches the user's intent.
/// 3. The asymmetry between Trigger and Debounce on debounce-suppression is
///    documented on `resolve` — Trigger came from a code path that already
///    bypassed the suppression check, so re-checking here would lose the
///    invariant.
#[derive(Debug, Default)]
struct PendingTrigger(Option<Action>);

impl PendingTrigger {
    fn new() -> Self {
        Self(None)
    }

    /// Stash a deferred action. Logs at trace when an existing stash is
    /// overwritten so a developer can correlate "the popup didn't fire"
    /// with a defer-overwrites-defer race.
    fn stash(&mut self, action: Action) {
        if let Some(prior) = self.0 {
            tracing::trace!(?prior, new = ?action, "PendingTrigger: overwriting prior stash");
        } else {
            tracing::trace!(new = ?action, "PendingTrigger: stashing");
        }
        self.0 = Some(action);
    }

    fn is_pending(&self) -> bool {
        self.0.is_some()
    }

    /// Clear the slot if a fresher non-Defer action superseded it. Logs at
    /// trace so a Defer(Trigger)→Debounce/Ignore demotion is visible at
    /// `RUST_LOG=trace`.
    fn clear_for_supersede(&mut self, reason: &'static str) {
        if let Some(prior) = self.0.take() {
            tracing::trace!(?prior, reason, "PendingTrigger: clearing on supersede");
        }
    }

    /// Drain the slot if non-empty. Returns None when auto_trigger is
    /// disabled. Returns None for a stashed Debounce when debounce is
    /// suppressed; a stashed Trigger always survives (it originated from
    /// a path — `has_pending_trigger` or `delay_ms == 0` — that already
    /// bypasses `debounce_suppressed`, so re-checking would lose the
    /// user-explicit-trigger-bypasses-debounce invariant).
    fn resolve(&mut self, auto_trigger_enabled: bool, debounce_suppressed: bool) -> Option<Action> {
        let action = self.0.take()?;
        if !auto_trigger_enabled {
            tracing::trace!(?action, "PendingTrigger: dropped (auto_trigger disabled)");
            return None;
        }
        match action {
            Action::Trigger => {
                tracing::trace!("PendingTrigger: fired Trigger");
                Some(Action::Trigger)
            }
            Action::Debounce => {
                if debounce_suppressed {
                    tracing::trace!("PendingTrigger: dropped Debounce (suppressed)");
                    None
                } else {
                    tracing::trace!("PendingTrigger: fired Debounce");
                    Some(Action::Debounce)
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverlayWriteOutcome {
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

pub(crate) fn write_overlay_if_current(
    handler: &Arc<Mutex<InputHandler>>,
    ticket: OverlayWriteTicket,
    render_buf: &[u8],
) -> std::io::Result<OverlayWriteOutcome> {
    write_overlay_if_current_using(handler, ticket, render_buf, |buf| {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(buf).and_then(|()| stdout.flush())
    })
}

fn write_overlay_if_current_using(
    handler: &Arc<Mutex<InputHandler>>,
    ticket: OverlayWriteTicket,
    render_buf: &[u8],
    write_render_buf: impl FnOnce(&[u8]) -> std::io::Result<()>,
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

    match write_render_buf(render_buf) {
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

        // Timer expired — fire trigger with bounded-block support.
        //
        // Phase 1: run suggest_sync under the handler lock and paint sync-only
        // results. If a high-priority async generator is pending and
        // render_block_ms > 0, we get back a `NeedsBlock` variant carrying
        // the channel receiver and sync geometry.
        let mut render_buf = Vec::new();
        let (prepared, render_ticket) = {
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
) {
    loop {
        notify.notified().await;
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

/// Background loop that wakes when the detail-box debounce window expires
/// and re-renders the popup so the box catches up to the settled selection.
///
/// Mirrors `dynamic_merge_loop`/`feedback_tick_loop`: notify-driven, locks
/// the handler briefly to render into a buffer, then writes through the
/// overlay-ownership ticket so a stale render is dropped instead of
/// overwriting fresh output.
async fn detail_redraw_loop(
    notify: Arc<Notify>,
    handler: Arc<Mutex<InputHandler>>,
    parser: Arc<Mutex<TerminalParser>>,
) {
    loop {
        notify.notified().await;
        if let Err(e) = detail_redraw_iteration(&handler, &parser, |buf| {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(buf).and_then(|()| stdout.flush())
        }) {
            tracing::debug!("detail redraw overlay write/flush failed: {e}");
            break;
        }
    }
}

fn detail_redraw_iteration(
    handler: &Arc<Mutex<InputHandler>>,
    parser: &Arc<Mutex<TerminalParser>>,
    write_render_buf: impl FnOnce(&[u8]) -> std::io::Result<()>,
) -> std::io::Result<OverlayWriteOutcome> {
    let mut buf: Vec<u8> = Vec::new();
    let render_ticket = {
        let mut h = match handler.lock() {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("detail redraw skipped (handler lock poisoned): {e}");
                return Ok(OverlayWriteOutcome::Empty);
            }
        };
        h.clear_detail_debounce_pending();
        // Only re-render when a popup is actually visible. The notify could
        // fire for a debounce timer that started on a now-dismissed popup;
        // render_for_detail_redraw is a no-op in that case.
        h.render_for_detail_redraw(parser, &mut buf);
        h.overlay_write_ticket()
    };
    write_overlay_if_current_using(handler, render_ticket, &buf, write_render_buf)
}

#[cfg(test)]
fn detail_redraw_iteration_to_writer(
    handler: &Arc<Mutex<InputHandler>>,
    parser: &Arc<Mutex<TerminalParser>>,
    writer: &mut dyn Write,
) -> std::io::Result<OverlayWriteOutcome> {
    detail_redraw_iteration(handler, parser, |buf| {
        writer.write_all(buf).and_then(|()| writer.flush())
    })
}

async fn feedback_tick_loop(notify: Arc<Notify>, handler: Arc<Mutex<InputHandler>>) {
    loop {
        notify.notified().await;
        let mut next_ms: u64 = 0;
        loop {
            if next_ms > 0 {
                tokio::time::sleep(Duration::from_millis(next_ms)).await;
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
    use gc_config::DescriptionBoxMode;
    use gc_suggest::{Suggestion, SuggestionKind, SuggestionSource};
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
    fn buffer_dirty_action_triggers_immediately_when_delay_zero_and_display_caught_up() {
        // OSC + redraw landed in same batch → buffer_pending_display=false
        assert_eq!(
            buffer_dirty_action(false, 0, true, false, false),
            BufferDirtyAction::Immediate(Action::Trigger)
        );
    }

    #[test]
    fn buffer_dirty_action_defers_when_delay_zero_and_display_pending() {
        // OSC arrived alone, redraw hasn't been processed yet
        assert_eq!(
            buffer_dirty_action(false, 0, true, false, true),
            BufferDirtyAction::Defer(Action::Trigger)
        );
    }

    #[test]
    fn buffer_dirty_action_ignores_when_auto_trigger_disabled() {
        // Exhaustive over has_pending_trigger × buffer_pending_display × delay_ms.
        // auto_disabled must dominate every combination — guards against a
        // future refactor that hoists has_pending_trigger above the
        // auto-disabled check.
        for has_pending in [false, true] {
            for pending in [false, true] {
                for delay_ms in [0u64, 150] {
                    for suppressed in [false, true] {
                        assert_eq!(
                            buffer_dirty_action(has_pending, delay_ms, false, suppressed, pending),
                            BufferDirtyAction::Ignore,
                            "auto-disabled must dominate (has_pending={has_pending}, \
                             delay_ms={delay_ms}, suppressed={suppressed}, \
                             pending_display={pending})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn buffer_dirty_action_ignores_when_debounce_suppressed_and_no_pending_trigger() {
        // Exhaustive over delay_ms × buffer_pending_display. The early
        // `if debounce_suppressed { return Ignore }` returns regardless of
        // delay_ms; iterating both cells guards against a future refactor that
        // hoists the delay_ms != 0 branch above the suppression check and
        // silently flips delay_ms=150 to Defer/Immediate(Debounce).
        for delay_ms in [0u64, 150] {
            for pending in [false, true] {
                assert_eq!(
                    buffer_dirty_action(false, delay_ms, true, true, pending),
                    BufferDirtyAction::Ignore,
                    "suppressed must dominate (delay_ms={delay_ms}, pending_display={pending})"
                );
            }
        }
    }

    #[test]
    fn buffer_dirty_action_debounces_when_delay_positive_and_display_caught_up() {
        assert_eq!(
            buffer_dirty_action(false, 150, true, false, false),
            BufferDirtyAction::Immediate(Action::Debounce)
        );
    }

    #[test]
    fn buffer_dirty_action_defers_to_debounce_when_delay_positive_and_display_pending() {
        assert_eq!(
            buffer_dirty_action(false, 150, true, false, true),
            BufferDirtyAction::Defer(Action::Debounce)
        );
    }

    #[test]
    fn buffer_dirty_action_prefers_pending_trigger_when_display_caught_up() {
        // Pending trigger from input handler bypasses debounce_suppressed
        assert_eq!(
            buffer_dirty_action(true, 150, true, true, false),
            BufferDirtyAction::Immediate(Action::Trigger)
        );
    }

    #[test]
    fn buffer_dirty_action_defers_pending_trigger_when_display_pending() {
        // Pending trigger but redraw hasn't applied yet — must defer to avoid stale cursor.
        assert_eq!(
            buffer_dirty_action(true, 150, true, true, true),
            BufferDirtyAction::Defer(Action::Trigger)
        );
    }

    #[test]
    fn pending_trigger_resolve_returns_none_when_empty() {
        let mut pending = PendingTrigger::new();
        assert_eq!(pending.resolve(true, false), None);
        assert!(!pending.is_pending());
    }

    #[test]
    fn pending_trigger_resolve_clears_slot_on_resolution() {
        let mut pending = PendingTrigger::new();
        pending.stash(Action::Trigger);
        let resolved = pending.resolve(true, false);
        assert_eq!(resolved, Some(Action::Trigger));
        assert!(
            !pending.is_pending(),
            "slot must be cleared so a stale entry cannot fire later"
        );
    }

    #[test]
    fn pending_trigger_resolve_drops_when_auto_trigger_disabled() {
        let mut pending = PendingTrigger::new();
        pending.stash(Action::Trigger);
        assert_eq!(pending.resolve(false, false), None);
        assert!(
            !pending.is_pending(),
            "disabled auto-trigger must drain the slot, not leave it for a later batch"
        );
    }

    #[test]
    fn pending_trigger_resolve_drops_debounce_when_auto_trigger_disabled() {
        // Pins that the auto_trigger check gates Debounce too — guards against
        // a refactor that moves the auto_trigger_enabled check inside the
        // match arms and silently lets stashed Debounce fire.
        let mut pending = PendingTrigger::new();
        pending.stash(Action::Debounce);
        assert_eq!(pending.resolve(false, false), None);
        assert!(!pending.is_pending());
    }

    #[test]
    fn pending_trigger_resolve_skips_debounce_when_suppressed() {
        let mut pending = PendingTrigger::new();
        pending.stash(Action::Debounce);
        assert_eq!(pending.resolve(true, true), None);
        assert!(!pending.is_pending());
    }

    #[test]
    fn pending_trigger_resolve_keeps_immediate_trigger_under_debounce_suppression() {
        // Action::Trigger was stashed by has_pending_trigger or delay_ms=0.
        // Both bypass debounce_suppressed, so the resolve path must too.
        let mut pending = PendingTrigger::new();
        pending.stash(Action::Trigger);
        assert_eq!(pending.resolve(true, true), Some(Action::Trigger));
        assert!(!pending.is_pending());
    }

    #[test]
    fn pending_trigger_stash_supersedes_prior_entry() {
        // A stale defer must never out-survive a fresh one — the second buffer
        // event reflects newer cursor geometry.
        let mut pending = PendingTrigger::new();
        pending.stash(Action::Trigger);
        assert!(pending.is_pending());
        pending.stash(Action::Debounce);
        assert_eq!(
            pending.resolve(true, false),
            Some(Action::Debounce),
            "newer Defer must overwrite the prior stash, not merge or be discarded"
        );
    }

    /// Exercise the dispatcher's `pending_trigger` reset wiring: the stdout
    /// task clears the slot on every non-Defer arm so a stashed entry from a
    /// prior iteration cannot survive a newer Trigger/Debounce/Ignore and
    /// fire spuriously when display catches up later.
    fn apply_buffer_dirty_action(slot: &mut PendingTrigger, action: BufferDirtyAction) {
        match action {
            BufferDirtyAction::Immediate(_) => slot.clear_for_supersede("immediate"),
            BufferDirtyAction::Defer(a) => slot.stash(a),
            BufferDirtyAction::Ignore => slot.clear_for_supersede("ignore"),
        }
    }

    #[test]
    fn pending_trigger_cleared_by_immediate_trigger() {
        let mut slot = PendingTrigger::new();
        slot.stash(Action::Trigger);
        apply_buffer_dirty_action(&mut slot, BufferDirtyAction::Immediate(Action::Trigger));
        assert!(
            !slot.is_pending(),
            "immediate Trigger must drain prior stash"
        );
    }

    #[test]
    fn pending_trigger_cleared_by_immediate_debounce() {
        let mut slot = PendingTrigger::new();
        slot.stash(Action::Trigger);
        apply_buffer_dirty_action(&mut slot, BufferDirtyAction::Immediate(Action::Debounce));
        assert!(
            !slot.is_pending(),
            "immediate Debounce must drain prior Trigger stash even though it demotes the user intent"
        );
    }

    #[test]
    fn pending_trigger_cleared_by_ignore() {
        let mut slot = PendingTrigger::new();
        slot.stash(Action::Trigger);
        apply_buffer_dirty_action(&mut slot, BufferDirtyAction::Ignore);
        assert!(!slot.is_pending(), "Ignore must drain prior stash");
    }

    #[test]
    fn pending_trigger_overwritten_by_defer() {
        let mut slot = PendingTrigger::new();
        slot.stash(Action::Trigger);
        apply_buffer_dirty_action(&mut slot, BufferDirtyAction::Defer(Action::Debounce));
        assert!(
            slot.is_pending(),
            "Defer must keep slot populated (with overwritten action)"
        );
        assert_eq!(slot.resolve(true, false), Some(Action::Debounce));
    }

    #[test]
    fn resolved_defer_debounce_drives_notify_one() {
        // Mirrors the stdout-task wiring: PendingTrigger::resolve →
        // Some(Debounce) → debounce_notify.notify_one(). A waiter parked on
        // .notified() must complete after that single notify_one.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");
        rt.block_on(async {
            let notify = Arc::new(Notify::new());
            let mut pending = PendingTrigger::new();
            pending.stash(Action::Debounce);
            let resolved = pending.resolve(true, false);
            assert_eq!(resolved, Some(Action::Debounce));
            if let Some(Action::Debounce) = resolved {
                notify.notify_one();
            }
            tokio::time::timeout(std::time::Duration::from_millis(100), notify.notified())
                .await
                .expect("notify_one must wake a parked waiter");
        });
    }

    use gc_parser::TerminalParser;

    fn make_state(rows: u16, cols: u16) -> TerminalParser {
        TerminalParser::new(rows, cols)
    }

    fn parser_with_buffer(buffer: &str) -> Arc<Mutex<TerminalParser>> {
        parser_with_buffer_and_size(buffer, 24, 80)
    }

    fn parser_with_buffer_and_size(
        buffer: &str,
        rows: u16,
        cols: u16,
    ) -> Arc<Mutex<TerminalParser>> {
        let parser = Arc::new(Mutex::new(TerminalParser::new(rows, cols)));
        let cursor = buffer.chars().count();
        let osc = format!("\x1b]7770;{cursor};{buffer}\x07");
        parser.lock().unwrap().process_bytes(osc.as_bytes());
        parser
    }

    fn detail_suggestion(text: &str, description: &str) -> Suggestion {
        Suggestion {
            text: text.to_string(),
            description: Some(description.to_string()),
            kind: SuggestionKind::Command,
            source: SuggestionSource::Spec,
            ..Default::default()
        }
    }

    fn write_custom_only_spec(dir: &std::path::Path) {
        std::fs::write(
            dir.join("custom-only.json"),
            r#"{"name":"custom-only","subcommands":[{"name":"local"}]}"#,
        )
        .unwrap();
    }

    fn proxy_resolution_test_handler(resolution: &SpecDirResolution) -> InputHandler {
        input_handler_for_resolution(resolution, TerminalProfile::for_ghostty())
            .expect("handler")
            .with_suggest_config(20, false, 0, false, true, false, false, 100)
    }

    fn trigger_texts(handler: &mut InputHandler, buffer: &str) -> Vec<String> {
        let parser = parser_with_buffer(buffer);
        let mut stdout = Vec::new();
        handler.trigger(&parser, &mut stdout);
        handler
            .current_suggestions()
            .iter()
            .map(|suggestion| suggestion.text.clone())
            .collect()
    }

    #[test]
    fn input_handler_resolution_includes_embedded_specs_when_requested() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
        let specs_dir = tempfile::TempDir::new().unwrap();
        write_custom_only_spec(specs_dir.path());
        let resolution = SpecDirResolution {
            dirs: vec![specs_dir.path().to_path_buf()],
            include_embedded: true,
        };
        let mut handler = proxy_resolution_test_handler(&resolution);

        let texts = trigger_texts(&mut handler, "git ");

        assert!(
            texts.iter().any(|text| text == "checkout"),
            "embedded git subcommands should be available with include_embedded=true: {texts:?}"
        );
    }

    #[test]
    fn input_handler_resolution_excludes_embedded_specs_when_not_requested() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
        let specs_dir = tempfile::TempDir::new().unwrap();
        write_custom_only_spec(specs_dir.path());
        let resolution = SpecDirResolution {
            dirs: vec![specs_dir.path().to_path_buf()],
            include_embedded: false,
        };
        let mut handler = proxy_resolution_test_handler(&resolution);

        let texts = trigger_texts(&mut handler, "git ");

        assert!(
            !texts.iter().any(|text| text == "checkout"),
            "embedded git subcommands should not be available with include_embedded=false: {texts:?}"
        );
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

    // The trigger() / handle_terminal_output() paths spawn a tokio task
    // for async generator dispatch when the engine resolves a known
    // command. Now that SuggestionEngine::new loads the embedded corpus
    // (so `git ` resolves), these tests need a runtime in scope to host
    // the spawn. The previous SpecStore::load_from_dirs(&[]) flow
    // produced an empty store, so dispatch never fired and a runtime
    // was unnecessary.
    #[test]
    fn write_overlay_if_current_discards_owned_state_on_stale_write() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
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
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
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
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
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

    #[tokio::test]
    async fn detail_redraw_iteration_clears_pending_and_commits_settled_detail() {
        let handler = InputHandler::new_with_embedded(
            &[std::path::PathBuf::from(".")],
            TerminalProfile::for_ghostty(),
            false,
        )
        .expect("handler")
        .with_popup_widths(20, 40)
        .with_description_box(DescriptionBoxMode::Side, 60, 5, 5)
        .test_with_visible_suggestions(
            vec![
                detail_suggestion(
                    "alpha",
                    "ALPHADETAIL alpha beta gamma delta epsilon zeta eta theta iota ALPHADONE",
                ),
                detail_suggestion(
                    "bravo",
                    "BRAVODETAIL alpha beta gamma delta epsilon zeta eta theta iota BRAVODONE",
                ),
            ],
            0,
        );
        let handler = Arc::new(Mutex::new(handler));
        let parser = parser_with_buffer_and_size("detail-redraw-test ", 24, 120);

        {
            let mut h = handler.lock().expect("handler");
            let mut first = Vec::new();
            h.render_for_detail_redraw(&parser, &mut first);
            let first_output = String::from_utf8_lossy(&first);
            assert!(
                first_output.contains("ALPHADETAIL"),
                "setup should render the initial detail: {first_output:?}"
            );
            let ticket = h.overlay_write_ticket();
            h.commit_overlay_write(ticket);
            assert_eq!(h.displayed_detail_idx_for_test(), Some(0));
        }

        let notify = handler.lock().expect("handler").detail_redraw_notify();
        let notified = notify.notified();
        {
            let mut h = handler.lock().expect("handler");
            let mut immediate = Vec::new();
            let forward = h.process_key(&KeyEvent::ArrowDown, &parser, &mut immediate);
            assert!(forward.is_empty());
            let immediate_output = String::from_utf8_lossy(&immediate);
            assert!(
                immediate_output.contains("ALPHADETAIL"),
                "in-window render should keep showing the previous detail: {immediate_output:?}"
            );
            assert!(
                !immediate_output.contains("BRAVODONE"),
                "in-window render must not show the settled detail yet: {immediate_output:?}"
            );
            assert!(h.detail_debounce_pending_for_test());
            let ticket = h.overlay_write_ticket();
            h.commit_overlay_write(ticket);
        }

        tokio::time::timeout(std::time::Duration::from_millis(200), notified)
            .await
            .expect("detail debounce timer should notify the proxy loop");

        let mut written = Vec::new();
        let outcome = detail_redraw_iteration_to_writer(&handler, &parser, &mut written)
            .expect("detail redraw write should succeed");
        let written_output = String::from_utf8_lossy(&written);
        assert_eq!(outcome, OverlayWriteOutcome::Written);
        assert!(
            written_output.contains("BRAVODONE"),
            "proxy redraw iteration should write the settled detail: {written_output:?}"
        );

        let h = handler.lock().expect("handler");
        assert!(!h.detail_debounce_pending_for_test());
        assert_eq!(h.displayed_detail_idx_for_test(), Some(1));
        assert!(
            h.overlay_write_ticket().render_token.is_none(),
            "overlay render token should be committed by the redraw write"
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
