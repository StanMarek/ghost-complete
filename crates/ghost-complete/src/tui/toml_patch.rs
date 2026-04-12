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

/// RAII guard that removes a temp file on Drop unless `disarm()` is called.
/// Ensures that a failed write/sync/rename doesn't leak a `.tmp-<pid>` sibling.
struct TempFileGuard {
    path: PathBuf,
    armed: bool,
}

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Atomically write `content` to `config_path`.
///
/// - If `config_path` is a symlink, the symlink is preserved: we resolve it via
///   `fs::canonicalize` and write to the resolved target so dotfile managers
///   (chezmoi, stow, etc.) keep their link intact.
/// - On Unix, the temp file is created with the target's mode from the start
///   (default `0o600` if the target doesn't exist) so sensitive content is
///   never visible to other users via the default umask.
/// - The temp file is cleaned up on any failure via `TempFileGuard`.
/// - Rename on the same filesystem is atomic, so a crash mid-write cannot
///   truncate the live config.
pub fn save_config(config_path: &Path, content: &str) -> Result<()> {
    // Resolve symlinks so we write to the real file and preserve the link.
    let target_path: PathBuf = match fs::symlink_metadata(config_path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(config_path)
            .with_context(|| format!("failed to resolve symlink {}", config_path.display()))?,
        _ => config_path.to_path_buf(),
    };

    let parent = target_path
        .parent()
        .context("config path has no parent directory")?;

    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create directories for {}", parent.display()))?;

    let file_name = target_path
        .file_name()
        .context("config path has no file name")?
        .to_string_lossy()
        .into_owned();
    let tmp_name = format!("{}.tmp-{}", file_name, process::id());
    let tmp_path = parent.join(&tmp_name);

    #[cfg(unix)]
    let target_mode: u32 = {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(&target_path)
            .ok()
            .map(|m| m.permissions().mode())
            .unwrap_or(0o600)
    };

    // Clean up the temp file if anything below fails.
    let guard = TempFileGuard::new(tmp_path.clone());

    // Open temp file with correct mode from the start (Unix), so sensitive
    // content never exists on disk under default umask perms.
    {
        #[cfg(unix)]
        let mut f = {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(target_mode)
                .open(&tmp_path)
                .with_context(|| format!("failed to create temp file {}", tmp_path.display()))?
        };
        #[cfg(not(unix))]
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .with_context(|| format!("failed to create temp file {}", tmp_path.display()))?;

        f.write_all(content.as_bytes())
            .with_context(|| format!("failed to write {}", tmp_path.display()))?;
        f.sync_all()
            .with_context(|| format!("failed to sync {}", tmp_path.display()))?;
    }

    fs::rename(&tmp_path, &target_path).with_context(|| {
        format!(
            "failed to rename {} to {}",
            tmp_path.display(),
            target_path.display()
        )
    })?;

    guard.disarm();
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

    #[test]
    fn save_config_leaves_no_tmp_sibling_after_success() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        fs::write(&path, "initial = 1\n").unwrap();

        save_config(&path, "updated = 2\n").unwrap();

        let leftovers: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no .tmp-* sibling should remain after successful save; found: {:?}",
            leftovers.iter().map(|e| e.path()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn save_config_cleans_up_tmp_on_rename_failure() {
        // Force rename failure by making the target a non-empty directory:
        // POSIX rename(file, non_empty_dir) fails with ENOTEMPTY/EISDIR.
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("config.toml");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("inhabitant"), "x").unwrap();

        let res = save_config(&target, "updated = 2\n");
        assert!(res.is_err(), "expected rename over non-empty dir to fail");

        let leftovers: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp file must be cleaned up on rename failure; found: {:?}",
            leftovers.iter().map(|e| e.path()).collect::<Vec<_>>()
        );
    }

    #[cfg(unix)]
    #[test]
    fn save_config_preserves_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("real");
        fs::create_dir(&real_dir).unwrap();
        let real_path = real_dir.join("config.toml");
        fs::write(&real_path, "initial = 1\n").unwrap();

        let link_dir = tmp.path().join("link");
        fs::create_dir(&link_dir).unwrap();
        let link_path = link_dir.join("config.toml");
        symlink(&real_path, &link_path).unwrap();

        save_config(&link_path, "updated = 2\n").unwrap();

        // Symlink must still exist and still point at the real file.
        let link_meta = fs::symlink_metadata(&link_path).unwrap();
        assert!(
            link_meta.file_type().is_symlink(),
            "symlink at config path must be preserved, not replaced with a regular file"
        );
        let link_target = fs::read_link(&link_path).unwrap();
        assert_eq!(link_target, real_path);

        // Real file content should be updated.
        let got = fs::read_to_string(&real_path).unwrap();
        assert_eq!(got, "updated = 2\n");

        // No temp file left in either directory.
        for dir in [&real_dir, &link_dir] {
            let leftovers: Vec<_> = fs::read_dir(dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
                .collect();
            assert!(
                leftovers.is_empty(),
                "no .tmp-* sibling should remain in {}; found: {:?}",
                dir.display(),
                leftovers.iter().map(|e| e.path()).collect::<Vec<_>>()
            );
        }
    }
}
