use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use toml_edit::{DocumentMut, Item, Value};

/// Patch a TOML document in-memory, preserving comments and formatting.
///
/// - `section` is dot-separated (e.g. `"suggest.providers"` → `[suggest.providers]`)
/// - `value_str` must be a valid TOML value literal (e.g. `"200"`, `"true"`, `"\"catppuccin\""`)
/// - Missing sections are created at the end of the document
/// - Missing keys in existing sections are inserted
pub fn patch_toml(source: &str, section: &str, key: &str, value_str: &str) -> Result<String> {
    let mut doc: DocumentMut = source.parse().context("failed to parse TOML source")?;

    let value: Value = value_str
        .parse()
        .with_context(|| format!("failed to parse value string: {value_str}"))?;

    // Navigate/create nested tables for each segment of the dot-separated section path.
    let segments: Vec<&str> = section.split('.').collect();

    let mut current: &mut Item = doc.as_item_mut();

    for segment in &segments {
        // Ensure the current item is a table (or implicit table).
        if current.is_none() {
            *current = Item::Table(toml_edit::Table::new());
        }

        // If the table doesn't contain this segment, insert an empty table.
        let tbl = current
            .as_table_like_mut()
            .with_context(|| format!("expected a table at segment '{segment}'"))?;

        if !tbl.contains_key(segment) {
            tbl.insert(segment, Item::Table(toml_edit::Table::new()));
        }

        current = tbl.get_mut(segment).expect("just inserted; must exist");
    }

    // `current` now points at the target section table.
    let tbl = current
        .as_table_like_mut()
        .with_context(|| format!("section '{section}' is not a table"))?;

    if let Some(existing) = tbl.get_mut(key) {
        // Update in-place so that the key's decoration (comments, spacing) is preserved.
        *existing = Item::Value(value);
    } else {
        tbl.insert(key, Item::Value(value));
    }

    Ok(doc.to_string())
}

/// Create a timestamped backup of `config_path`.
///
/// The backup is written alongside the original file with a `.backup.<unix_secs>` suffix.
/// Returns the path of the created backup.
pub fn backup_config(config_path: &Path) -> Result<PathBuf> {
    anyhow::ensure!(
        config_path.exists(),
        "config file does not exist: {}",
        config_path.display()
    );

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time before UNIX epoch")?
        .as_secs();

    let backup_name = format!(
        "{}.backup.{}",
        config_path
            .file_name()
            .context("config path has no file name")?
            .to_string_lossy(),
        ts
    );

    let backup_path = config_path
        .parent()
        .context("config path has no parent directory")?
        .join(backup_name);

    fs::copy(config_path, &backup_path).with_context(|| {
        format!(
            "failed to copy {} to {}",
            config_path.display(),
            backup_path.display()
        )
    })?;

    Ok(backup_path)
}

/// Write `content` to `config_path`, creating parent directories as needed.
pub fn save_config(config_path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directories for {}", parent.display()))?;
    }

    fs::write(config_path, content)
        .with_context(|| format!("failed to write {}", config_path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_existing_scalar() {
        let input = "# My config\n[trigger]\ndelay_ms = 150\n";
        let patched = patch_toml(input, "trigger", "delay_ms", "200").unwrap();
        assert!(patched.contains("delay_ms = 200"));
        assert!(patched.contains("# My config"), "comment preserved");
    }

    #[test]
    fn patch_creates_missing_key() {
        let input = "[trigger]\ndelay_ms = 150\n";
        let patched = patch_toml(input, "trigger", "auto_trigger", "false").unwrap();
        assert!(patched.contains("auto_trigger = false"));
        assert!(patched.contains("delay_ms = 150"));
    }

    #[test]
    fn patch_creates_missing_section() {
        let input = "[trigger]\ndelay_ms = 150\n";
        let patched = patch_toml(input, "popup", "borders", "true").unwrap();
        assert!(patched.contains("[popup]"));
        assert!(patched.contains("borders = true"));
    }

    #[test]
    fn patch_nested_section() {
        let input = "[suggest]\nmax_results = 50\n";
        let patched = patch_toml(input, "suggest.providers", "git", "false").unwrap();
        assert!(patched.contains("[suggest.providers]"));
        assert!(patched.contains("git = false"));
    }

    #[test]
    fn patch_preserves_comments() {
        let input = "# Ghost Complete Config\n\n[theme]\n# My custom theme\npreset = \"dark\"\n";
        let patched = patch_toml(input, "theme", "preset", "\"catppuccin\"").unwrap();
        assert!(patched.contains("# Ghost Complete Config"));
        assert!(patched.contains("# My custom theme"));
        assert!(patched.contains("preset = \"catppuccin\""));
    }

    #[test]
    fn patch_empty_source() {
        let patched = patch_toml("", "trigger", "delay_ms", "200").unwrap();
        assert!(patched.contains("[trigger]"));
        assert!(patched.contains("delay_ms = 200"));
    }
}
