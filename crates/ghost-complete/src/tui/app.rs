use gc_config::GhostConfig;
use std::path::PathBuf;

use super::fields::{self, FieldMeta, SECTIONS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sections,
    Fields,
}

#[derive(Debug, Clone)]
pub enum EditState {
    /// Not editing — just browsing.
    None,
    /// Editing a text/number field. Buffer holds the in-progress value.
    Text { buffer: String, cursor: usize },
}

pub struct App {
    /// Loaded config (source of truth for current values).
    pub config: GhostConfig,
    /// Raw TOML source (for comment-preserving patching).
    pub raw_toml: String,
    /// Path to the config file.
    pub config_path: PathBuf,
    /// Whether a backup has been created this session.
    pub backup_created: bool,
    /// All field metadata (computed once at startup).
    pub all_fields: Vec<FieldMeta>,
    /// Current focus pane.
    pub focus: Focus,
    /// Selected section index (into SECTIONS).
    pub section_idx: usize,
    /// Selected field index within the current section.
    pub field_idx: usize,
    /// Current edit state.
    pub edit_state: EditState,
    /// Validation errors (field key -> error message).
    pub errors: Vec<(String, String)>,
    /// Whether unsaved changes exist.
    pub dirty: bool,
    /// Whether the user wants to quit.
    pub should_quit: bool,
}

impl App {
    pub fn new(config: GhostConfig, raw_toml: String, config_path: PathBuf) -> Self {
        Self {
            config,
            raw_toml,
            config_path,
            backup_created: false,
            all_fields: fields::all_fields(),
            focus: Focus::Sections,
            section_idx: 0,
            field_idx: 0,
            edit_state: EditState::None,
            errors: Vec::new(),
            dirty: false,
            should_quit: false,
        }
    }

    pub fn current_section(&self) -> &'static str {
        SECTIONS[self.section_idx]
    }

    pub fn current_section_fields(&self) -> Vec<&FieldMeta> {
        let section = self.current_section();
        self.all_fields
            .iter()
            .filter(|f| f.section == section)
            .collect()
    }

    /// Get the current value of a field from the config as a display string.
    pub fn field_value(&self, field: &FieldMeta) -> String {
        let Ok(root) = toml::Value::try_from(&self.config) else {
            return field.default.to_string();
        };

        // Navigate through section parts (e.g. "suggest.providers" -> ["suggest", "providers"])
        let parts: Vec<&str> = field.section.split('.').collect();
        let mut current = &root;
        for part in &parts {
            let toml::Value::Table(table) = current else {
                return field.default.to_string();
            };
            let Some(next) = table.get(*part) else {
                return field.default.to_string();
            };
            current = next;
        }

        let toml::Value::Table(table) = current else {
            return field.default.to_string();
        };

        match table.get(field.key) {
            Some(v) => format_toml_value(v),
            None => field.default.to_string(),
        }
    }
}

fn format_toml_value(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(format_toml_value).collect();
            format!("[{}]", items.join(", "))
        }
        _ => v.to_string(),
    }
}
