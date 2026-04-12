use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};
use toml_edit::{DocumentMut, Item, Value};

/// Patch a TOML document in-memory, preserving comments and formatting.
///
/// - `section` is dot-separated (e.g. `"suggest.providers"` → `[suggest.providers]`)
/// - `value` is a `toml_edit::Value` built by the caller
/// - Missing sections are created at the end of the document
/// - Missing keys in existing sections are inserted
/// - When replacing an existing key, the value's `decor` (leading/trailing
///   whitespace, inline comments) is preserved on the replacement.
pub fn patch_toml(source: &str, section: &str, key: &str, value: Value) -> Result<String> {
    let mut doc: DocumentMut = source.parse().context("failed to parse TOML source")?;

    // Navigate/create nested tables for each segment of the dot-separated section path.
    let segments: Vec<&str> = section.split('.').collect();

    let mut current: &mut Item = doc.as_item_mut();

    for segment in &segments {
        if current.is_none() {
            *current = Item::Table(toml_edit::Table::new());
        }

        let tbl = current
            .as_table_like_mut()
            .with_context(|| format!("expected a table at segment '{segment}'"))?;

        if !tbl.contains_key(segment) {
            tbl.insert(segment, Item::Table(toml_edit::Table::new()));
        }

        current = tbl.get_mut(segment).expect("just inserted; must exist");
    }

    let tbl = current
        .as_table_like_mut()
        .with_context(|| format!("section '{section}' is not a table"))?;

    if let Some(existing) = tbl.get_mut(key) {
        // Preserve the existing value's decor (inline comments, spacing) on replacement.
        let existing_decor = existing.as_value().map(|v| v.decor().clone());
        let mut new_value = value;
        if let Some(decor) = existing_decor {
            *new_value.decor_mut() = decor;
        }
        *existing = Item::Value(new_value);
    } else {
        tbl.insert(key, Item::Value(value));
    }

    Ok(doc.to_string())
}

/// Build a `toml_edit::Value` for a string field. Handles escape-sensitive
/// characters (`"`, `\`) safely — unlike `format!("\"{s}\"")`.
pub fn string_value(s: &str) -> Value {
    Value::from(s)
}

/// Parse a raw TOML value literal (for non-string fields: numbers, bools, arrays).
pub fn parse_value(value_str: &str) -> Result<Value> {
    value_str
        .parse()
        .with_context(|| format!("failed to parse value string: {value_str}"))
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

/// Atomically write `content` to `config_path`.
///
/// Writes to a sibling temp file (`<name>.tmp-<pid>`), fsyncs, preserves the
/// original file's mode if one existed, then renames over the target. Rename
/// on the same filesystem is atomic, so a crash mid-write cannot truncate the
/// live config.
pub fn save_config(config_path: &Path, content: &str) -> Result<()> {
    let parent = config_path
        .parent()
        .context("config path has no parent directory")?;

    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create directories for {}", parent.display()))?;

    let file_name = config_path
        .file_name()
        .context("config path has no file name")?
        .to_string_lossy()
        .into_owned();
    let tmp_name = format!("{}.tmp-{}", file_name, process::id());
    let tmp_path = parent.join(&tmp_name);

    let original_mode = fs::metadata(config_path).ok().map(|m| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            m.permissions().mode()
        }
        #[cfg(not(unix))]
        {
            let _ = m;
            0
        }
    });

    // Scope the file handle so it's closed (and flushed) before rename.
    {
        let mut f = fs::File::create(&tmp_path)
            .with_context(|| format!("failed to create temp file {}", tmp_path.display()))?;
        f.write_all(content.as_bytes())
            .with_context(|| format!("failed to write {}", tmp_path.display()))?;
        f.sync_all()
            .with_context(|| format!("failed to sync {}", tmp_path.display()))?;
    }

    #[cfg(unix)]
    if let Some(mode) = original_mode {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(mode))
            .with_context(|| format!("failed to set permissions on {}", tmp_path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = original_mode;

    fs::rename(&tmp_path, config_path).with_context(|| {
        format!(
            "failed to rename {} to {}",
            tmp_path.display(),
            config_path.display()
        )
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Value {
        parse_value(s).unwrap()
    }

    #[test]
    fn patch_existing_scalar() {
        let input = "# My config\n[trigger]\ndelay_ms = 150\n";
        let patched = patch_toml(input, "trigger", "delay_ms", v("200")).unwrap();
        assert!(patched.contains("delay_ms = 200"));
        assert!(patched.contains("# My config"), "comment preserved");
    }

    #[test]
    fn patch_creates_missing_key() {
        let input = "[trigger]\ndelay_ms = 150\n";
        let patched = patch_toml(input, "trigger", "auto_trigger", v("false")).unwrap();
        assert!(patched.contains("auto_trigger = false"));
        assert!(patched.contains("delay_ms = 150"));
    }

    #[test]
    fn patch_creates_missing_section() {
        let input = "[trigger]\ndelay_ms = 150\n";
        let patched = patch_toml(input, "popup", "borders", v("true")).unwrap();
        assert!(patched.contains("[popup]"));
        assert!(patched.contains("borders = true"));
    }

    #[test]
    fn patch_nested_section() {
        let input = "[suggest]\nmax_results = 50\n";
        let patched = patch_toml(input, "suggest.providers", "git", v("false")).unwrap();
        assert!(patched.contains("[suggest.providers]"));
        assert!(patched.contains("git = false"));
    }

    #[test]
    fn patch_preserves_comments() {
        let input = "# Ghost Complete Config\n\n[theme]\n# My custom theme\npreset = \"dark\"\n";
        let patched = patch_toml(input, "theme", "preset", string_value("catppuccin")).unwrap();
        assert!(patched.contains("# Ghost Complete Config"));
        assert!(patched.contains("# My custom theme"));
        assert!(patched.contains("preset = \"catppuccin\""));
    }

    #[test]
    fn patch_empty_source() {
        let patched = patch_toml("", "trigger", "delay_ms", v("200")).unwrap();
        assert!(patched.contains("[trigger]"));
        assert!(patched.contains("delay_ms = 200"));
    }

    #[test]
    fn patch_preserves_inline_comment_on_edited_value() {
        let input = "[trigger]\ndelay_ms = 150 # why 150\n";
        let patched = patch_toml(input, "trigger", "delay_ms", v("200")).unwrap();
        assert!(patched.contains("200"));
        assert!(
            patched.contains("# why 150"),
            "inline comment should be preserved; got: {patched}"
        );
    }

    #[test]
    fn patch_preserves_leading_comment_on_key() {
        let input = "[theme]\n# this is the current preset\npreset = \"dark\"\n";
        let patched = patch_toml(input, "theme", "preset", string_value("catppuccin")).unwrap();
        assert!(patched.contains("# this is the current preset"));
        assert!(patched.contains("preset = \"catppuccin\""));
    }

    #[test]
    fn patch_nested_preserves_inline_comment() {
        let input = "[suggest.providers]\ngit = true # enable git\n";
        let patched = patch_toml(input, "suggest.providers", "git", v("false")).unwrap();
        assert!(patched.contains("git = false"));
        assert!(patched.contains("# enable git"));
    }

    #[test]
    fn string_value_escapes_quotes_and_backslashes() {
        // Hostile inputs that would break naive formatting.
        let input = "[theme]\nselected = \"reverse\"\n";
        let patched = patch_toml(
            input,
            "theme",
            "selected",
            string_value("has \"quote\" and \\back"),
        )
        .unwrap();
        // Reparsing must round-trip safely.
        let reparsed: DocumentMut = patched.parse().unwrap();
        let got = reparsed["theme"]["selected"].as_str().unwrap();
        assert_eq!(got, "has \"quote\" and \\back");
    }

    #[test]
    fn save_config_is_atomic_and_preserves_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        fs::write(&path, "initial = 1\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }

        save_config(&path, "updated = 2\n").unwrap();

        let got = fs::read_to_string(&path).unwrap();
        assert_eq!(got, "updated = 2\n");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "original file mode should be preserved");
        }
    }
}
