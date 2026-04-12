use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use gc_config::GhostConfig;
use toml_edit::Value;

use super::app::{App, EditState, Focus};
use super::fields::{self, FieldType};
use super::toml_patch;

/// Convert a char-based cursor position to a byte index in a string.
/// Cursor 0 = byte 0, cursor N = byte offset of the Nth char.
fn char_to_byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(byte, _)| byte)
        .unwrap_or(s.len())
}

/// Handle a key event, dispatching to navigation or edit mode as appropriate.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    // Global keys (always active)
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
        save(app);
        return;
    }

    // Edit mode
    if let EditState::Text {
        ref mut buffer,
        ref mut cursor,
    } = app.edit_state
    {
        match key.code {
            KeyCode::Esc => {
                app.edit_state = EditState::None;
            }
            KeyCode::Enter => {
                apply_edit(app);
            }
            KeyCode::Backspace => {
                if *cursor > 0 {
                    *cursor -= 1;
                    let byte_idx = char_to_byte_index(buffer, *cursor);
                    buffer.remove(byte_idx);
                }
            }
            KeyCode::Left => {
                *cursor = cursor.saturating_sub(1);
            }
            KeyCode::Right => {
                if *cursor < buffer.chars().count() {
                    *cursor += 1;
                }
            }
            KeyCode::Char(c) => {
                let byte_idx = char_to_byte_index(buffer, *cursor);
                buffer.insert(byte_idx, c);
                *cursor += 1;
            }
            _ => {}
        }
        return;
    }

    // Navigation mode (EditState::None)
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => {
            app.should_quit = true;
        }
        KeyCode::Tab | KeyCode::BackTab => {
            app.focus = match app.focus {
                Focus::Sections => Focus::Fields,
                Focus::Fields => Focus::Sections,
            };
            app.field_idx = 0;
        }
        KeyCode::Up | KeyCode::Char('k') => match app.focus {
            Focus::Sections => {
                app.section_idx = app.section_idx.saturating_sub(1);
                app.field_idx = 0;
            }
            Focus::Fields => {
                app.field_idx = app.field_idx.saturating_sub(1);
            }
        },
        KeyCode::Down | KeyCode::Char('j') => match app.focus {
            Focus::Sections => {
                let max = super::fields::SECTIONS.len().saturating_sub(1);
                if app.section_idx < max {
                    app.section_idx += 1;
                    app.field_idx = 0;
                }
            }
            Focus::Fields => {
                let max = app.current_section_fields().len().saturating_sub(1);
                if app.field_idx < max {
                    app.field_idx += 1;
                }
            }
        },
        KeyCode::Enter | KeyCode::Char(' ') => match app.focus {
            Focus::Sections => {
                app.focus = Focus::Fields;
                app.field_idx = 0;
            }
            Focus::Fields => {
                start_edit(app);
            }
        },
        _ => {}
    }
}

fn start_edit(app: &mut App) {
    let fields = app.current_section_fields();
    let Some(field) = fields.get(app.field_idx) else {
        return;
    };

    let current_value = app.field_value(field);
    let section = field.section;
    let key = field.key;

    match field.field_type {
        FieldType::Bool => {
            // Toggle immediately
            let new_val = if current_value == "true" {
                Value::from(false)
            } else {
                Value::from(true)
            };
            apply_field_change(app, section, key, new_val);
        }
        FieldType::Enum(options) => {
            // Cycle to next option
            let current_idx = options
                .iter()
                .position(|o| *o == current_value)
                .unwrap_or(0);
            let next_idx = (current_idx + 1) % options.len();
            apply_field_change(
                app,
                section,
                key,
                toml_patch::string_value(options[next_idx]),
            );
        }
        _ => {
            // Enter text edit mode
            let buffer = current_value.clone();
            let cursor = buffer.chars().count();
            app.edit_state = EditState::Text { buffer, cursor };
        }
    }
}

fn apply_edit(app: &mut App) {
    let EditState::Text { ref buffer, .. } = app.edit_state else {
        return;
    };
    let buffer = buffer.clone();

    let fields = app.current_section_fields();
    let Some(field) = fields.get(app.field_idx) else {
        app.edit_state = EditState::None;
        return;
    };

    let section = field.section;
    let key = field.key;
    let field_key = format!("{section}.{key}");

    if field.field_type == FieldType::StyleString
        && fields::supports_inherit(field)
        && buffer == super::app::INHERIT_SENTINEL
    {
        apply_field_removal(app, section, key);
        app.edit_state = EditState::None;
        return;
    }

    // Build the TOML value. Strings go through `Value::from` to handle
    // embedded quotes/backslashes; numeric/array literals are parsed.
    let value_result = match field.field_type {
        FieldType::String | FieldType::StyleString => Ok(toml_patch::string_value(&buffer)),
        _ => toml_patch::parse_value(&buffer),
    };

    match value_result {
        Ok(value) => {
            apply_field_change(app, section, key, value);
        }
        Err(e) => {
            set_error(app, &field_key, format!("Invalid value: {e}"));
        }
    }
    app.edit_state = EditState::None;
}

fn apply_field_change(app: &mut App, section: &str, key: &str, toml_value: Value) {
    let field_key = format!("{section}.{key}");

    match toml_patch::patch_toml(&app.raw_toml, section, key, toml_value) {
        Ok(new_toml) => commit_toml_update(app, field_key, new_toml),
        Err(e) => {
            let msg = format!("Patch failed: {e}");
            set_error(app, &field_key, msg);
        }
    }
}

fn apply_field_removal(app: &mut App, section: &str, key: &str) {
    let field_key = format!("{section}.{key}");

    match toml_patch::remove_key(&app.raw_toml, section, key) {
        Ok(new_toml) => commit_toml_update(app, field_key, new_toml),
        Err(e) => {
            let msg = format!("Patch failed: {e}");
            set_error(app, &field_key, msg);
        }
    }
}

fn commit_toml_update(app: &mut App, field_key: String, new_toml: String) {
    if new_toml == app.raw_toml {
        app.errors.retain(|(k, _)| k != &field_key);
        return;
    }

    match toml::from_str::<GhostConfig>(&new_toml) {
        Ok(new_config) => {
            if let Err(e) = new_config.theme.validate() {
                set_error(app, &field_key, format!("Invalid value: {e}"));
                return;
            }

            let mut normalized = new_config.clone();
            normalized.normalize();
            if normalized.popup.max_visible != new_config.popup.max_visible {
                set_error(
                    app,
                    &field_key,
                    format!(
                        "value {} out of range: would be clamped to {}",
                        new_config.popup.max_visible, normalized.popup.max_visible
                    ),
                );
                return;
            }
            if normalized.suggest.max_results != new_config.suggest.max_results {
                set_error(
                    app,
                    &field_key,
                    format!(
                        "value {} out of range: would be clamped to {}",
                        new_config.suggest.max_results, normalized.suggest.max_results
                    ),
                );
                return;
            }

            app.raw_toml = new_toml;
            app.config = new_config;
            app.dirty = true;
            app.errors.retain(|(k, _)| k != &field_key);
        }
        Err(e) => {
            set_error(app, &field_key, format!("Invalid value: {e}"));
        }
    }
}

fn set_error(app: &mut App, key: &str, msg: String) {
    if let Some((_, existing)) = app.errors.iter_mut().find(|(k, _)| k == key) {
        *existing = msg;
    } else {
        app.errors.push((key.to_string(), msg));
    }
}

fn save(app: &mut App) {
    if !app.dirty {
        return;
    }

    // Create backup on first save if file exists
    if !app.backup_created && app.config_path.exists() {
        match toml_patch::backup_config(&app.config_path) {
            Ok(_) => {
                app.backup_created = true;
            }
            Err(e) => {
                set_error(app, "save", format!("Backup failed: {e}"));
                return;
            }
        }
    }

    // Validate theme before saving
    if let Err(e) = app.config.theme.validate() {
        set_error(app, "save", format!("Theme invalid: {e}"));
        return;
    }

    match toml_patch::save_config(&app.config_path, &app.raw_toml) {
        Ok(()) => {
            app.dirty = false;
            // Clear save-related errors
            app.errors.retain(|(k, _)| k != "save");
        }
        Err(e) => {
            set_error(app, "save", format!("Save failed: {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_app(raw_toml: &str) -> App {
        let config: GhostConfig = toml::from_str(raw_toml).unwrap_or_default();
        App::new(
            config,
            raw_toml.to_string(),
            PathBuf::from("/tmp/unused.toml"),
        )
    }

    #[test]
    fn apply_field_change_rejects_out_of_range_max_visible() {
        let mut app = make_app("[popup]\nmax_visible = 8\n");
        apply_field_change(&mut app, "popup", "max_visible", Value::from(100_000));
        assert!(!app.dirty, "out-of-range value must not be committed");
        let err = app
            .errors
            .iter()
            .find(|(k, _)| k == "popup.max_visible")
            .expect("error should be recorded");
        assert!(err.1.contains("out of range"), "got: {}", err.1);
        assert!(err.1.contains("clamped"), "got: {}", err.1);
    }

    #[test]
    fn apply_field_change_rejects_out_of_range_max_results() {
        let mut app = make_app("[suggest]\nmax_results = 50\n");
        apply_field_change(&mut app, "suggest", "max_results", Value::from(20_000));
        assert!(!app.dirty);
        assert!(app
            .errors
            .iter()
            .any(|(k, v)| k == "suggest.max_results" && v.contains("out of range")));
    }

    #[test]
    fn apply_field_change_accepts_in_range() {
        let mut app = make_app("[popup]\nmax_visible = 8\n");
        apply_field_change(&mut app, "popup", "max_visible", Value::from(12));
        assert!(app.dirty, "in-range value must commit");
        assert!(app.errors.is_empty());
    }

    #[test]
    fn apply_field_change_rejects_zero_max_visible() {
        let mut app = make_app("[popup]\nmax_visible = 8\n");
        apply_field_change(&mut app, "popup", "max_visible", Value::from(0));
        assert!(!app.dirty, "zero must not be committed");
        assert!(app
            .errors
            .iter()
            .any(|(k, v)| k == "popup.max_visible" && v.contains("clamped")));
    }

    #[test]
    fn apply_field_change_rejects_invalid_theme_style() {
        let mut app = make_app("[theme]\npreset = \"dark\"\n");
        apply_field_change(
            &mut app,
            "theme",
            "selected",
            toml_patch::string_value("not-a-valid-style-token"),
        );

        assert!(!app.dirty, "invalid theme style must not be committed");
        assert!(app
            .errors
            .iter()
            .any(|(k, v)| k == "theme.selected" && v.contains("invalid theme.selected")));
    }

    #[test]
    fn apply_edit_can_restore_theme_inherit() {
        let mut app = make_app("[theme]\npreset = \"dark\"\nselected = \"reverse\"\n");
        app.section_idx = super::super::fields::SECTIONS
            .iter()
            .position(|section| *section == "theme")
            .expect("theme section should exist");
        app.field_idx = app
            .current_section_fields()
            .iter()
            .position(|field| field.key == "selected")
            .expect("theme.selected should exist");
        app.edit_state = EditState::Text {
            buffer: "<inherit>".to_string(),
            cursor: "<inherit>".chars().count(),
        };

        apply_edit(&mut app);

        assert!(app.dirty, "restoring inherit should mark the app dirty");
        assert_eq!(app.config.theme.selected, None);
        assert!(
            !app.raw_toml.contains("selected ="),
            "theme override should be removed from raw TOML"
        );
    }

    #[test]
    fn apply_edit_exits_edit_mode_when_restoring_theme_inherit() {
        let mut app = make_app("[theme]\npreset = \"dark\"\nselected = \"reverse\"\n");
        app.section_idx = super::super::fields::SECTIONS
            .iter()
            .position(|section| *section == "theme")
            .expect("theme section should exist");
        app.field_idx = app
            .current_section_fields()
            .iter()
            .position(|field| field.key == "selected")
            .expect("theme.selected should exist");
        app.edit_state = EditState::Text {
            buffer: "<inherit>".to_string(),
            cursor: "<inherit>".chars().count(),
        };

        apply_edit(&mut app);

        assert!(matches!(app.edit_state, EditState::None));
    }

    #[test]
    fn apply_edit_keeps_clean_when_theme_already_inherits() {
        let mut app = make_app("[theme]\npreset = \"dark\"\n");
        app.section_idx = super::super::fields::SECTIONS
            .iter()
            .position(|section| *section == "theme")
            .expect("theme section should exist");
        app.field_idx = app
            .current_section_fields()
            .iter()
            .position(|field| field.key == "selected")
            .expect("theme.selected should exist");
        app.edit_state = EditState::Text {
            buffer: "<inherit>".to_string(),
            cursor: "<inherit>".chars().count(),
        };

        apply_edit(&mut app);

        assert!(!app.dirty, "no-op inherit edit should stay clean");
        assert_eq!(app.raw_toml, "[theme]\npreset = \"dark\"\n");
        assert!(matches!(app.edit_state, EditState::None));
    }

    #[test]
    fn set_error_replaces_existing_save_message() {
        let mut app = make_app("");
        app.errors
            .push(("save".to_string(), "Backup failed: old".to_string()));

        set_error(&mut app, "save", "Save failed: new".to_string());

        let save_errors: Vec<_> = app.errors.iter().filter(|(key, _)| key == "save").collect();
        assert_eq!(
            save_errors.len(),
            1,
            "save error should be replaced in-place"
        );
        assert_eq!(save_errors[0].1, "Save failed: new");
    }
}
