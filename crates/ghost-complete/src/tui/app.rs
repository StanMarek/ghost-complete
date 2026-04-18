use gc_config::GhostConfig;
use std::path::PathBuf;
use std::time::SystemTime;

use super::fields::{self, FieldMeta, SECTIONS};

pub const INHERIT_SENTINEL: &str = "<inherit>";

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

/// Modal prompts that must be dismissed before any other interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    /// User tried to quit with unsaved changes. y = discard, n = cancel,
    /// s = save-then-quit.
    ConfirmQuit,
    /// Config file changed on disk after we loaded it; saving would clobber.
    /// r = reload from disk, o = overwrite anyway, c = cancel.
    FileChangedOnDisk,
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
    /// mtime of `config_path` at load time (or None if the file did not exist).
    /// Used to detect external edits that would otherwise be silently clobbered.
    pub loaded_mtime: Option<SystemTime>,
    /// Active modal prompt, if any. Blocks normal key handling while set.
    pub prompt: Option<Prompt>,
}

impl App {
    pub fn new(config: GhostConfig, raw_toml: String, config_path: PathBuf) -> Self {
        let loaded_mtime = std::fs::metadata(&config_path)
            .ok()
            .and_then(|m| m.modified().ok());
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
            loaded_mtime,
            prompt: None,
        }
    }

    /// Refresh `loaded_mtime` to the current on-disk mtime. Call after a
    /// successful save so the freshly-written mtime becomes the new baseline.
    pub fn refresh_loaded_mtime(&mut self) {
        self.loaded_mtime = std::fs::metadata(&self.config_path)
            .ok()
            .and_then(|m| m.modified().ok());
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
        if fields::supports_inherit(field) && !self.has_explicit_field_value(field) {
            return INHERIT_SENTINEL.to_string();
        }

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

    fn has_explicit_field_value(&self, field: &FieldMeta) -> bool {
        let Ok(root) = toml::from_str::<toml::Value>(&self.raw_toml) else {
            return false;
        };

        let parts: Vec<&str> = field.section.split('.').collect();
        let mut current = &root;
        for part in &parts {
            let toml::Value::Table(table) = current else {
                return false;
            };
            let Some(next) = table.get(*part) else {
                return false;
            };
            current = next;
        }

        let toml::Value::Table(table) = current else {
            return false;
        };

        table.contains_key(field.key)
    }
}

fn format_toml_value(v: &toml::Value) -> String {
    match v {
        // Scalars are rendered bare so the field row reads naturally; arrays
        // delegate to a TOML-literal formatter so a round-trip through the
        // text editor stays lossless (e.g. `auto_chars = [' ', '/']`).
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Array(arr) => format_toml_array(arr),
        _ => v.to_string(),
    }
}

/// Render a TOML array as a valid TOML literal: each element is quoted/formatted
/// per TOML rules so the resulting string parses back to an equivalent array.
fn format_toml_array(arr: &[toml::Value]) -> String {
    let items: Vec<String> = arr.iter().map(format_array_element).collect();
    format!("[{}]", items.join(", "))
}

fn format_array_element(v: &toml::Value) -> String {
    // Use `toml_edit::Value` for element formatting — it handles quoting,
    // escape sequences, and single-char strings without losing whitespace.
    let edit_value = toml_value_to_edit_value(v);
    edit_value.to_string().trim().to_string()
}

fn toml_value_to_edit_value(v: &toml::Value) -> toml_edit::Value {
    match v {
        toml::Value::String(s) => toml_edit::Value::from(s.as_str()),
        toml::Value::Integer(i) => toml_edit::Value::from(*i),
        toml::Value::Boolean(b) => toml_edit::Value::from(*b),
        toml::Value::Float(f) => toml_edit::Value::from(*f),
        toml::Value::Array(arr) => {
            let mut out = toml_edit::Array::new();
            for item in arr {
                out.push(toml_value_to_edit_value(item));
            }
            toml_edit::Value::Array(out)
        }
        // Datetimes and inline tables: fall back to TOML's own Display.
        other => {
            // Parse via toml_edit to preserve the literal form.
            other
                .to_string()
                .parse::<toml_edit::Value>()
                .unwrap_or_else(|_| toml_edit::Value::from(other.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme_field(key: &str) -> FieldMeta {
        fields::all_fields()
            .into_iter()
            .find(|field| field.section == "theme" && field.key == key)
            .expect("theme field should exist")
    }

    #[test]
    fn field_value_marks_inherited_theme_override() {
        let raw_toml = "[theme]\npreset = \"dark\"\n";
        let config: GhostConfig = toml::from_str(raw_toml).unwrap();
        let app = App::new(
            config,
            raw_toml.to_string(),
            PathBuf::from("/tmp/config.toml"),
        );

        assert_eq!(app.field_value(&theme_field("selected")), "<inherit>");
    }

    #[test]
    fn field_value_preserves_explicit_empty_theme_override() {
        let raw_toml = "[theme]\npreset = \"dark\"\nselected = \"\"\n";
        let config: GhostConfig = toml::from_str(raw_toml).unwrap();
        let app = App::new(
            config,
            raw_toml.to_string(),
            PathBuf::from("/tmp/config.toml"),
        );

        assert_eq!(app.field_value(&theme_field("selected")), "");
    }

    #[test]
    fn format_toml_array_round_trips_char_array() {
        // Regression: whitespace-containing char array used to render as
        // `[ , /, -]` (unquoted), which failed to reparse as TOML.
        let raw_toml = "[trigger]\nauto_chars = [' ', '/', '-']\n";
        let value: toml::Value = toml::from_str(raw_toml).unwrap();
        let arr = value
            .get("trigger")
            .and_then(|t| t.get("auto_chars"))
            .and_then(|v| v.as_array())
            .expect("auto_chars array should exist");

        let rendered = format_toml_array(arr);
        let reparsed: toml_edit::Value = rendered
            .parse()
            .expect("rendered array must parse as TOML value");
        let reparsed_arr = reparsed
            .as_array()
            .expect("reparsed value must be an array");

        assert_eq!(reparsed_arr.len(), 3);
        assert_eq!(reparsed_arr.get(0).and_then(|v| v.as_str()), Some(" "));
        assert_eq!(reparsed_arr.get(1).and_then(|v| v.as_str()), Some("/"));
        assert_eq!(reparsed_arr.get(2).and_then(|v| v.as_str()), Some("-"));
    }

    #[test]
    fn format_toml_array_round_trips_string_array() {
        let arr = vec![
            toml::Value::String("a b".to_string()),
            toml::Value::String("with \"quote\"".to_string()),
        ];
        let rendered = format_toml_array(&arr);
        let reparsed: toml_edit::Value = rendered.parse().unwrap();
        let reparsed_arr = reparsed.as_array().unwrap();

        assert_eq!(reparsed_arr.get(0).and_then(|v| v.as_str()), Some("a b"));
        assert_eq!(
            reparsed_arr.get(1).and_then(|v| v.as_str()),
            Some("with \"quote\"")
        );
    }
}
