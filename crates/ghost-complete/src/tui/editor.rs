use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use gc_config::GhostConfig;

use super::app::{App, EditState, Focus};
use super::fields::FieldType;
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
                "false"
            } else {
                "true"
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
            let toml_value = format!("\"{}\"", options[next_idx]);
            apply_field_change(app, section, key, &toml_value);
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

    // Format as TOML value
    let toml_value = match field.field_type {
        FieldType::String | FieldType::StyleString => format!("\"{}\"", buffer),
        FieldType::CharArray | FieldType::StringArray => buffer.clone(),
        _ => buffer.clone(), // numbers, etc.
    };

    apply_field_change(app, section, key, &toml_value);
    app.edit_state = EditState::None;
}

fn apply_field_change(app: &mut App, section: &str, key: &str, toml_value: &str) {
    let field_key = format!("{section}.{key}");

    match toml_patch::patch_toml(&app.raw_toml, section, key, toml_value) {
        Ok(new_toml) => {
            // Validate by parsing the patched TOML as a full config
            match toml::from_str::<GhostConfig>(&new_toml) {
                Ok(new_config) => {
                    app.raw_toml = new_toml;
                    app.config = new_config;
                    app.dirty = true;
                    // Clear errors for this key
                    app.errors.retain(|(k, _)| k != &field_key);
                }
                Err(e) => {
                    app.errors.push((field_key, format!("Invalid value: {e}")));
                }
            }
        }
        Err(e) => {
            app.errors.push((field_key, format!("Patch failed: {e}")));
        }
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
                app.errors
                    .push(("save".to_string(), format!("Backup failed: {e}")));
                return;
            }
        }
    }

    // Validate theme before saving
    if let Err(e) = app.config.theme.validate() {
        app.errors
            .push(("save".to_string(), format!("Theme invalid: {e}")));
        return;
    }

    match toml_patch::save_config(&app.config_path, &app.raw_toml) {
        Ok(()) => {
            app.dirty = false;
            // Clear save-related errors
            app.errors.retain(|(k, _)| k != "save");
        }
        Err(e) => {
            app.errors
                .push(("save".to_string(), format!("Save failed: {e}")));
        }
    }
}
