//! Config hot-reload via filesystem watching.
//!
//! Watches `config.toml` for modifications and live-updates the handler's
//! theme, keybindings, trigger chars, and popup dimensions without restarting.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use gc_config::GhostConfig;
use gc_overlay::{parse_style, PopupTheme};
use notify::{EventKind, RecursiveMode, Watcher};

use crate::handler::{InputHandler, Keybindings};

/// Spawn a background task that watches `config_path` for modifications and
/// hot-reloads runtime-configurable fields into the handler.
///
/// Uses `notify::RecommendedWatcher` with a simple time-based debounce
/// (200ms minimum between reloads) to coalesce rapid write events.
///
/// On reload failure (parse error, invalid theme, etc.) a warning is logged
/// and the previous config is kept. The proxy never crashes from a bad config
/// edit.
pub fn spawn_config_watcher(config_path: PathBuf, handler: Arc<Mutex<InputHandler>>) -> Result<()> {
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

    tokio::task::spawn_blocking(move || {
        // Keep watcher alive for the lifetime of this blocking task.
        let _watcher = watcher;
        let mut last_reload = Instant::now() - std::time::Duration::from_secs(1);
        let debounce = std::time::Duration::from_millis(200);

        for event_result in rx {
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

            let config_path_str = config_path.to_str().unwrap_or("");
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

            let theme = match build_popup_theme(&resolved_theme) {
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
            {
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
                );
            }

            tracing::info!("config reloaded successfully");
        }
    });

    Ok(())
}

/// Build a `PopupTheme` from a resolved `ThemeConfig`, parsing each style string.
fn build_popup_theme(resolved: &gc_config::ThemeConfig) -> Result<PopupTheme> {
    Ok(PopupTheme {
        selected_on: parse_style(&resolved.selected)
            .map_err(|e| anyhow::anyhow!("invalid theme.selected: {e}"))?,
        description_on: parse_style(&resolved.description)
            .map_err(|e| anyhow::anyhow!("invalid theme.description: {e}"))?,
        match_highlight_on: parse_style(&resolved.match_highlight)
            .map_err(|e| anyhow::anyhow!("invalid theme.match_highlight: {e}"))?,
        item_text_on: parse_style(&resolved.item_text)
            .map_err(|e| anyhow::anyhow!("invalid theme.item_text: {e}"))?,
        scrollbar_on: parse_style(&resolved.scrollbar)
            .map_err(|e| anyhow::anyhow!("invalid theme.scrollbar: {e}"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_popup_theme_valid() {
        let theme_config = gc_config::ThemeConfig {
            selected: "reverse".into(),
            description: "dim".into(),
            match_highlight: "bold".into(),
            item_text: "".into(),
            scrollbar: "dim".into(),
            ..Default::default()
        };
        let result = build_popup_theme(&theme_config);
        assert!(result.is_ok());
    }
}
