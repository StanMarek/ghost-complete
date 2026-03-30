//! TOML configuration, keybinding definitions, and color themes.
//!
//! Reads from `~/.config/ghost-complete/config.toml` with serde deserialization
//! and sensible defaults for all fields.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Returns `~/.config/ghost-complete`, ignoring macOS `~/Library/Application Support/`.
pub fn config_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config").join("ghost-complete"))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GhostConfig {
    pub trigger: TriggerConfig,
    pub popup: PopupConfig,
    pub suggest: SuggestConfig,
    pub paths: PathsConfig,
    pub keybindings: KeybindingsConfig,
    pub theme: ThemeConfig,
    pub experimental: ExperimentalConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExperimentalConfig {
    pub multi_terminal: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PopupConfig {
    pub max_visible: usize,
}

impl Default for PopupConfig {
    fn default() -> Self {
        Self { max_visible: 10 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SuggestConfig {
    pub max_results: usize,
    pub max_history_results: usize,
    pub providers: ProvidersConfig,
}

impl Default for SuggestConfig {
    fn default() -> Self {
        Self {
            max_results: 50,
            max_history_results: 5,
            providers: ProvidersConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProvidersConfig {
    pub commands: bool,
    pub filesystem: bool,
    pub specs: bool,
    pub git: bool,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            commands: true,
            filesystem: true,
            specs: true,
            git: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub preset: String,
    pub selected: String,
    pub description: String,
    pub match_highlight: String,
    pub item_text: String,
    pub scrollbar: String,
}

impl ThemeConfig {
    /// Resolve preset base + field overrides into a fully populated ThemeConfig.
    pub fn resolve(&self) -> Result<ThemeConfig> {
        let preset_name = if self.preset.is_empty() {
            "dark"
        } else {
            &self.preset
        };
        let base = preset_values(preset_name)?;
        Ok(ThemeConfig {
            preset: self.preset.clone(),
            selected: if self.selected.is_empty() {
                base.selected
            } else {
                self.selected.clone()
            },
            description: if self.description.is_empty() {
                base.description
            } else {
                self.description.clone()
            },
            match_highlight: if self.match_highlight.is_empty() {
                base.match_highlight
            } else {
                self.match_highlight.clone()
            },
            item_text: if self.item_text.is_empty() {
                base.item_text
            } else {
                self.item_text.clone()
            },
            scrollbar: if self.scrollbar.is_empty() {
                base.scrollbar
            } else {
                self.scrollbar.clone()
            },
        })
    }
}

fn preset_values(name: &str) -> Result<ThemeConfig> {
    let theme = match name {
        "dark" => ThemeConfig {
            selected: "reverse".into(),
            description: "dim".into(),
            match_highlight: "bold".into(),
            scrollbar: "dim".into(),
            ..Default::default()
        },
        "light" => ThemeConfig {
            selected: "fg:#1e1e2e bg:#dce0e8 bold".into(),
            description: "fg:#6c6f85".into(),
            match_highlight: "fg:#d20f39 bold".into(),
            scrollbar: "fg:#9ca0b0".into(),
            ..Default::default()
        },
        "catppuccin" => ThemeConfig {
            selected: "fg:#cdd6f4 bg:#585b70 bold".into(),
            description: "fg:#6c7086".into(),
            match_highlight: "fg:#f9e2af bold".into(),
            scrollbar: "fg:#585b70".into(),
            ..Default::default()
        },
        "material-darker" => ThemeConfig {
            selected: "fg:#eeffff bg:#424242 bold".into(),
            description: "fg:#616161".into(),
            match_highlight: "fg:#ffcb6b bold".into(),
            scrollbar: "fg:#424242".into(),
            ..Default::default()
        },
        _ => bail!(
            "unknown theme preset: {:?} (valid: dark, light, catppuccin, material-darker)",
            name
        ),
    };
    Ok(theme)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
        assert_eq!(config.suggest.max_results, 50);
        assert_eq!(config.suggest.max_history_results, 5);
        assert!(config.suggest.providers.commands);
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
        assert_eq!(config.theme.preset, "");
        assert_eq!(config.theme.selected, "");
        assert_eq!(config.theme.description, "");
        assert_eq!(config.theme.match_highlight, "");
        assert_eq!(config.theme.item_text, "");
        assert_eq!(config.theme.scrollbar, "");
        assert!(!config.experimental.multi_terminal);
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

[suggest]
max_results = 100
max_history_results = 3

[suggest.providers]
commands = true
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
        assert_eq!(config.suggest.max_results, 100);
        assert_eq!(config.suggest.max_history_results, 3);
        assert!(config.suggest.providers.commands);
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
        assert_eq!(config.theme.description, "");
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

    #[test]
    fn test_theme_new_field_defaults() {
        let config = GhostConfig::default();
        assert_eq!(config.theme.match_highlight, "");
        assert_eq!(config.theme.item_text, "");
        assert_eq!(config.theme.scrollbar, "");
    }

    #[test]
    fn test_partial_theme_new_fields() {
        let toml_str = r#"
[theme]
match_highlight = "underline"
scrollbar = "fg:#555555"
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.theme.match_highlight, "underline");
        assert_eq!(config.theme.scrollbar, "fg:#555555");
        assert_eq!(config.theme.selected, "");
        assert_eq!(config.theme.description, "");
        assert_eq!(config.theme.item_text, "");
    }

    #[test]
    fn test_resolve_no_preset_uses_dark() {
        let config = ThemeConfig::default();
        let resolved = config.resolve().unwrap();
        assert_eq!(resolved.selected, "reverse");
        assert_eq!(resolved.description, "dim");
        assert_eq!(resolved.match_highlight, "bold");
        assert_eq!(resolved.item_text, "");
        assert_eq!(resolved.scrollbar, "dim");
    }

    #[test]
    fn test_resolve_catppuccin_preset() {
        let config = ThemeConfig {
            preset: "catppuccin".into(),
            ..Default::default()
        };
        let resolved = config.resolve().unwrap();
        assert_eq!(resolved.selected, "fg:#cdd6f4 bg:#585b70 bold");
        assert_eq!(resolved.description, "fg:#6c7086");
        assert_eq!(resolved.match_highlight, "fg:#f9e2af bold");
        assert_eq!(resolved.item_text, "");
        assert_eq!(resolved.scrollbar, "fg:#585b70");
    }

    #[test]
    fn test_resolve_preset_with_field_override() {
        let config = ThemeConfig {
            preset: "catppuccin".into(),
            match_highlight: "underline".into(),
            ..Default::default()
        };
        let resolved = config.resolve().unwrap();
        // Override wins
        assert_eq!(resolved.match_highlight, "underline");
        // Rest from preset
        assert_eq!(resolved.selected, "fg:#cdd6f4 bg:#585b70 bold");
        assert_eq!(resolved.description, "fg:#6c7086");
    }

    #[test]
    fn test_resolve_invalid_preset_errors() {
        let config = ThemeConfig {
            preset: "nonexistent".into(),
            ..Default::default()
        };
        assert!(config.resolve().is_err());
    }

    #[test]
    fn test_resolve_material_darker_preset() {
        let config = ThemeConfig {
            preset: "material-darker".into(),
            ..Default::default()
        };
        let resolved = config.resolve().unwrap();
        assert_eq!(resolved.selected, "fg:#eeffff bg:#424242 bold");
        assert_eq!(resolved.description, "fg:#616161");
        assert_eq!(resolved.match_highlight, "fg:#ffcb6b bold");
        assert_eq!(resolved.scrollbar, "fg:#424242");
    }

    #[test]
    fn test_resolve_light_preset() {
        let config = ThemeConfig {
            preset: "light".into(),
            ..Default::default()
        };
        let resolved = config.resolve().unwrap();
        assert_eq!(resolved.selected, "fg:#1e1e2e bg:#dce0e8 bold");
        assert_eq!(resolved.description, "fg:#6c6f85");
        assert_eq!(resolved.match_highlight, "fg:#d20f39 bold");
        assert_eq!(resolved.item_text, "");
        assert_eq!(resolved.scrollbar, "fg:#9ca0b0");
    }

    #[test]
    fn test_legacy_providers_history_field_ignored() {
        let toml_str = r#"
[suggest.providers]
history = false
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        // Field is silently ignored; max_history_results keeps its default
        assert_eq!(config.suggest.max_history_results, 5);
    }

    #[test]
    fn test_experimental_defaults_to_off() {
        let config = GhostConfig::default();
        assert!(!config.experimental.multi_terminal);
    }

    #[test]
    fn test_experimental_multi_terminal_enabled() {
        let toml_str = r#"
[experimental]
multi_terminal = true
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert!(config.experimental.multi_terminal);
    }

    #[test]
    fn test_experimental_missing_uses_default() {
        let toml_str = r#"
[popup]
max_visible = 5
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.experimental.multi_terminal);
    }
}
