//! Config hot-reload via filesystem watching.
//!
//! Watches `config.toml` for modifications and live-updates the handler's
//! theme, keybindings, trigger chars, and popup dimensions without restarting.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use gc_config::GhostConfig;
use gc_overlay::{parse_style, PopupTheme};
use notify::{EventKind, RecursiveMode, Watcher};

use crate::handler::{InputHandler, Keybindings};

/// Handle returned by [`spawn_config_watcher`] that allows the caller to
/// signal the watcher thread to shut down.
pub struct ConfigWatcherHandle {
    shutdown: Arc<AtomicBool>,
    join: tokio::task::JoinHandle<()>,
}

impl ConfigWatcherHandle {
    /// Signal the watcher thread to exit and abort the wrapping task.
    /// The blocking thread checks the flag on each `recv_timeout` cycle
    /// (≤500ms), so it will terminate promptly after this call.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.join.abort();
    }
}

impl Drop for ConfigWatcherHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.join.abort();
    }
}

/// Spawn a background task that watches `config_path` for modifications and
/// hot-reloads runtime-configurable fields into the handler.
///
/// Uses `notify::RecommendedWatcher` with a simple time-based debounce
/// (200ms minimum between reloads) to coalesce rapid write events.
///
/// On reload failure (parse error, invalid theme, etc.) a warning is logged
/// and the previous config is kept. The proxy never crashes from a bad config
/// edit.
pub fn spawn_config_watcher(
    config_path: PathBuf,
    handler: Arc<Mutex<InputHandler>>,
) -> Result<ConfigWatcherHandle> {
    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();

    let watch_dir = config_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let mut watcher = notify::RecommendedWatcher::new(tx, notify::Config::default())?;
    watcher.watch(&watch_dir, RecursiveMode::NonRecursive)?;

    let config_file_name = config_path
        .file_name()
        .map(|f| f.to_os_string())
        .unwrap_or_default();

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = Arc::clone(&shutdown);

    let join = tokio::task::spawn_blocking(move || {
        // Keep watcher alive for the lifetime of this blocking task.
        let _watcher = watcher;
        let mut last_reload = Instant::now() - std::time::Duration::from_secs(1);
        let debounce = Duration::from_millis(200);
        let poll_interval = Duration::from_millis(500);

        loop {
            // Check shutdown flag before blocking on recv
            if shutdown_clone.load(Ordering::Acquire) {
                break;
            }

            let event_result = match rx.recv_timeout(poll_interval) {
                Ok(result) => result,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            };
            let event = match event_result {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("config watcher error: {e}");
                    continue;
                }
            };

            // Only react to modify/create events that touch our config file.
            let dominated_by_config = match event.kind {
                EventKind::Modify(_) | EventKind::Create(_) => event.paths.iter().any(|p| {
                    p.file_name()
                        .map(|f| f == config_file_name)
                        .unwrap_or(false)
                }),
                _ => false,
            };

            if !dominated_by_config {
                continue;
            }

            // Debounce: skip if we reloaded very recently.
            let now = Instant::now();
            if now.duration_since(last_reload) < debounce {
                continue;
            }
            last_reload = now;

            tracing::info!("config.toml changed, reloading...");

            // If the config path contains non-UTF-8 bytes, we can't pass
            // it through the str-based GhostConfig::load API. Skipping the
            // reload is strictly better than silently substituting an
            // empty string (which would dispatch load() to its "no path"
            // fallback and reload the wrong file).
            let Some(config_path_str) = config_path.to_str() else {
                tracing::warn!(
                    "config reload skipped: path is not valid UTF-8: {}",
                    config_path.display()
                );
                continue;
            };
            let config = match GhostConfig::load(Some(config_path_str)) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("config reload failed (parse): {e}");
                    continue;
                }
            };

            // Resolve theme
            let resolved_theme = match config.theme.resolve() {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("config reload failed (theme preset): {e}");
                    continue;
                }
            };

            let theme = match build_popup_theme(
                &resolved_theme,
                config.popup.borders,
                config.popup.spinner,
                config.popup.show_provider_errors,
            ) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("config reload failed (theme styles): {e}");
                    continue;
                }
            };

            // Resolve keybindings
            let keybindings = match Keybindings::from_config(&config.keybindings) {
                Ok(kb) => kb,
                Err(e) => {
                    tracing::warn!("config reload failed (keybindings): {e}");
                    continue;
                }
            };

            // Apply to handler
            let cleanup = {
                let mut h = match handler.lock() {
                    Ok(h) => h,
                    Err(e) => {
                        tracing::warn!("config reload skipped (handler lock poisoned): {e}");
                        continue;
                    }
                };
                h.update_config(
                    theme,
                    keybindings,
                    &config.trigger.auto_chars,
                    config.popup.max_visible,
                    config.popup.feedback_dismiss_ms,
                    config.trigger.auto_trigger,
                )
            };
            if !cleanup.is_empty() {
                let mut stdout = std::io::stdout().lock();
                let _ = stdout.write_all(&cleanup);
                let _ = stdout.flush();
            }

            tracing::info!("config reloaded successfully");
            tracing::debug!(
                "note: changes to delay_ms, max_results, providers, spec_dirs, \
                 and [experimental] require a restart to take effect"
            );
        }
    });

    Ok(ConfigWatcherHandle { shutdown, join })
}

/// Build a `PopupTheme` from a [`gc_config::ResolvedTheme`] (preset merged
/// with user overrides), parsing each style string.
fn build_popup_theme(
    resolved: &gc_config::ResolvedTheme,
    borders: bool,
    spinner: bool,
    show_provider_errors: bool,
) -> Result<PopupTheme> {
    Ok(PopupTheme {
        selected_on: parse_style(&resolved.selected)
            .map_err(|e| anyhow::anyhow!("invalid theme.selected: {e}"))?,
        description_on: parse_style(&resolved.description)
            .map_err(|e| anyhow::anyhow!("invalid theme.description: {e}"))?,
        feedback_loading_on: parse_style(&resolved.feedback_loading)
            .map_err(|e| anyhow::anyhow!("invalid theme.feedback_loading: {e}"))?,
        feedback_empty_on: parse_style(&resolved.feedback_empty)
            .map_err(|e| anyhow::anyhow!("invalid theme.feedback_empty: {e}"))?,
        feedback_error_on: parse_style(&resolved.feedback_error)
            .map_err(|e| anyhow::anyhow!("invalid theme.feedback_error: {e}"))?,
        match_highlight_on: parse_style(&resolved.match_highlight)
            .map_err(|e| anyhow::anyhow!("invalid theme.match_highlight: {e}"))?,
        item_text_on: parse_style(&resolved.item_text)
            .map_err(|e| anyhow::anyhow!("invalid theme.item_text: {e}"))?,
        scrollbar_on: parse_style(&resolved.scrollbar)
            .map_err(|e| anyhow::anyhow!("invalid theme.scrollbar: {e}"))?,
        border_on: parse_style(&resolved.border)
            .map_err(|e| anyhow::anyhow!("invalid theme.border: {e}"))?,
        borders,
        spinner,
        show_provider_errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_popup_theme_valid() {
        let resolved = gc_config::ResolvedTheme {
            selected: "reverse".into(),
            description: "dim".into(),
            match_highlight: "bold".into(),
            item_text: String::new(),
            scrollbar: "dim".into(),
            border: "dim".into(),
            feedback_loading: "dim".into(),
            feedback_empty: "dim".into(),
            feedback_error: "dim fg:#f38ba8".into(),
        };
        let result = build_popup_theme(&resolved, true, true, false);
        assert!(result.is_ok());
        assert!(result.unwrap().borders);
    }
}
