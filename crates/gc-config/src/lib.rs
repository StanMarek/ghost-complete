use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GhostConfig {
    pub trigger: TriggerConfig,
    pub popup: PopupConfig,
    pub suggest: SuggestConfig,
    pub paths: PathsConfig,
}

impl Default for GhostConfig {
    fn default() -> Self {
        Self {
            trigger: TriggerConfig::default(),
            popup: PopupConfig::default(),
            suggest: SuggestConfig::default(),
            paths: PathsConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TriggerConfig {
    pub auto_chars: Vec<char>,
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            auto_chars: vec![' ', '/', '-', '.'],
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
    pub providers: ProvidersConfig,
}

impl Default for SuggestConfig {
    fn default() -> Self {
        Self {
            max_results: 50,
            max_history_entries: 10_000,
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
pub struct PathsConfig {
    pub spec_dirs: Vec<String>,
}

impl Default for PathsConfig {
    fn default() -> Self {
        Self {
            spec_dirs: Vec::new(),
        }
    }
}

impl GhostConfig {
    pub fn load(path: Option<&str>) -> Result<Self> {
        let config_path = match path {
            Some(p) => PathBuf::from(p),
            None => {
                let config_dir = dirs::config_dir()
                    .unwrap_or_else(|| PathBuf::from("~/.config"));
                config_dir.join("ghost-complete").join("config.toml")
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
        assert_eq!(config.popup.max_visible, 10);
        assert_eq!(config.popup.min_width, 20);
        assert_eq!(config.popup.max_width, 60);
        assert_eq!(config.suggest.max_results, 50);
        assert_eq!(config.suggest.max_history_entries, 10_000);
        assert!(config.suggest.providers.commands);
        assert!(config.suggest.providers.history);
        assert!(config.suggest.providers.filesystem);
        assert!(config.suggest.providers.specs);
        assert!(config.suggest.providers.git);
        assert!(config.paths.spec_dirs.is_empty());
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
    fn test_full_config_parses() {
        let toml_str = r#"
[trigger]
auto_chars = [' ', '/']

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
"#;
        let config: GhostConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.trigger.auto_chars, vec![' ', '/']);
        assert_eq!(config.popup.max_visible, 15);
        assert_eq!(config.popup.min_width, 25);
        assert_eq!(config.popup.max_width, 80);
        assert_eq!(config.suggest.max_results, 100);
        assert_eq!(config.suggest.max_history_entries, 5000);
        assert!(config.suggest.providers.commands);
        assert!(!config.suggest.providers.history);
        assert!(!config.suggest.providers.git);
        assert_eq!(config.paths.spec_dirs, vec!["/usr/local/share/ghost-complete/specs"]);
    }
}
