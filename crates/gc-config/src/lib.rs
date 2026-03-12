//! TOML configuration, keybinding definitions, and color themes.
//!
//! Reads from `~/.config/ghost-complete/config.toml` with serde deserialization
//! and sensible defaults for all fields.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Returns `~/.config/ghost-complete`, ignoring macOS `~/Library/Application Support/`.
pub fn config_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config").join("ghost-complete"))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct GhostConfig {
    pub trigger: TriggerConfig,
    pub popup: PopupConfig,
    pub suggest: SuggestConfig,
    pub paths: PathsConfig,
    pub keybindings: KeybindingsConfig,
    pub theme: ThemeConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct KeybindingsConfig {
    pub accept: String,
    pub accept_and_enter: String,
    pub dismiss: String,
    pub navigate_up: String,
    pub navigate_down: String,
    pub trigger: String,
}

impl Default for KeybindingsConfig {
    fn default() -> Self {
        Self {
            accept: "tab".to_string(),
            accept_and_enter: "enter".to_string(),
            dismiss: "escape".to_string(),
            navigate_up: "arrow_up".to_string(),
            navigate_down: "arrow_down".to_string(),
            trigger: "ctrl+/".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TriggerConfig {
    pub auto_chars: Vec<char>,
    pub delay_ms: u64,
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            auto_chars: vec![' ', '/', '-', '.'],
            delay_ms: 150,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PopupConfig {
    pub max_visible: usize,
    pub min_width: u16,
    pub max_width: u16,
}

impl Default for PopupConfig {
    fn default() -> Self {
        Self {
            max_visible: 10,
            min_width: 20,
            max_width: 60,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SuggestConfig {
    pub max_results: usize,
    pub max_history_entries: usize,
    pub generator_timeout_ms: u64,
    pub providers: ProvidersConfig,
}

impl Default for SuggestConfig {
    fn default() -> Self {
        Self {
            max_results: 50,
            max_history_entries: 10_000,
            generator_timeout_ms: 5000,
            providers: ProvidersConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ProvidersConfig {
    pub commands: bool,
    pub history: bool,
    pub filesystem: bool,
    pub specs: bool,
    pub git: bool,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            commands: true,
            history: true,
            filesystem: true,
            specs: true,
            git: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub selected: String,
    pub description: String,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            selected: "reverse".to_string(),
            description: "dim".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PathsConfig {
    pub spec_dirs: Vec<String>,
}

impl GhostConfig {
    pub fn load(path: Option<&str>) -> Result<Self> {
        let config_path = match path {
            Some(p) => PathBuf::from(p),
            None => {
                let dir = config_dir().unwrap_or_else(|| PathBuf::from("."));
                dir.join("config.toml")
            }
        };

        if !config_path.exists() {
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(&config_path)
            .with_context(|| format!("failed to read config file: {}", config_path.display()))?;

        let config: GhostConfig = toml::from_str(&contents)
            .with_context(|| format!("failed to parse config file: {}", config_path.display()))?;

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_default_config_matches_hardcoded() {
        let config = GhostConfig::default();
        assert_eq!(config.trigger.auto_chars, vec![' ', '/', '-', '.']);
        assert_eq!(config.trigger.delay_ms, 150);
        assert_eq!(config.popup.max_visible, 10);
        assert_eq!(config.popup.min_width, 20);
        assert_eq!(config.popup.max_width, 60);
        assert_eq!(config.suggest.max_results, 50);
        assert_eq!(config.suggest.max_history_entries, 10_000);
        assert_eq!(config.suggest.generator_timeout_ms, 5000);
        assert!(config.suggest.providers.commands);
        assert!(config.suggest.providers.history);
        assert!(config.suggest.providers.filesystem);
        assert!(config.suggest.providers.specs);
        assert!(config.suggest.providers.git);
        assert!(config.paths.spec_dirs.is_empty());
        assert_eq!(config.keybindings.accept, "tab");
        assert_eq!(config.keybindings.accept_and_enter, "enter");
        assert_eq!(config.keybindings.dismiss, "escape");
        assert_eq!(config.keybindings.navigate_up, "arrow_up");
        assert_eq!(config.keybindings.navigate_down, "arrow_down");
        assert_eq!(config.keybindings.trigger, "ctrl+/");
        assert_eq!(config.theme.selected, "reverse");
        assert_eq!(config.theme.description, "dim");
    }

    #[test]
    fn test_parse_partial_toml() {
        let toml_str = r#"
[popup]
max_visible = 5
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.popup.max_visible, 5);
        // Everything else should be default
        assert_eq!(config.popup.min_width, 20);
        assert_eq!(config.popup.max_width, 60);
        assert_eq!(config.trigger.auto_chars, vec![' ', '/', '-', '.']);
        assert_eq!(config.suggest.max_results, 50);
    }

    #[test]
    fn test_missing_file_returns_default() {
        let config = GhostConfig::load(Some("/nonexistent/path/config.toml")).unwrap();
        assert_eq!(config.popup.max_visible, 10);
    }

    #[test]
    fn test_malformed_toml_returns_error() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "this is not [valid toml = {{}}").unwrap();
        let result = GhostConfig::load(Some(tmp.path().to_str().unwrap()));
        assert!(result.is_err());
    }

    #[test]
    fn test_partial_keybindings_override() {
        let toml_str = r#"
[keybindings]
accept = "enter"
navigate_up = "ctrl+space"
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.keybindings.accept, "enter");
        assert_eq!(config.keybindings.navigate_up, "ctrl+space");
        // Unset fields keep defaults
        assert_eq!(config.keybindings.accept_and_enter, "enter");
        assert_eq!(config.keybindings.dismiss, "escape");
        assert_eq!(config.keybindings.navigate_down, "arrow_down");
        assert_eq!(config.keybindings.trigger, "ctrl+/");
    }

    #[test]
    fn test_full_config_parses() {
        let toml_str = r#"
[trigger]
auto_chars = [' ', '/']
delay_ms = 200

[popup]
max_visible = 15
min_width = 25
max_width = 80

[suggest]
max_results = 100
max_history_entries = 5000

[suggest.providers]
commands = true
history = false
filesystem = true
specs = true
git = false

[paths]
spec_dirs = ["/usr/local/share/ghost-complete/specs"]

[keybindings]
accept = "enter"
accept_and_enter = "tab"
dismiss = "escape"
navigate_up = "arrow_up"
navigate_down = "arrow_down"
trigger = "ctrl+space"

[theme]
selected = "bold"
description = "dim"
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.trigger.auto_chars, vec![' ', '/']);
        assert_eq!(config.trigger.delay_ms, 200);
        assert_eq!(config.theme.selected, "bold");
        assert_eq!(config.theme.description, "dim");
        assert_eq!(config.popup.max_visible, 15);
        assert_eq!(config.popup.min_width, 25);
        assert_eq!(config.popup.max_width, 80);
        assert_eq!(config.suggest.max_results, 100);
        assert_eq!(config.suggest.max_history_entries, 5000);
        assert!(config.suggest.providers.commands);
        assert!(!config.suggest.providers.history);
        assert!(!config.suggest.providers.git);
        assert_eq!(
            config.paths.spec_dirs,
            vec!["/usr/local/share/ghost-complete/specs"]
        );
        assert_eq!(config.keybindings.accept, "enter");
        assert_eq!(config.keybindings.accept_and_enter, "tab");
    }

    #[test]
    fn test_partial_theme_override() {
        let toml_str = r#"
[theme]
selected = "bold fg:255"
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.theme.selected, "bold fg:255");
        // Unset field keeps default
        assert_eq!(config.theme.description, "dim");
    }

    #[test]
    fn test_full_theme_config() {
        let toml_str = r#"
[theme]
selected = "fg:255 bg:236"
description = "dim underline"
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.theme.selected, "fg:255 bg:236");
        assert_eq!(config.theme.description, "dim underline");
    }
}
