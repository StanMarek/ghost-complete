use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use toml_edit::DocumentMut;

use crate::sanitize::sanitize_preserving_whitespace;

/// Resolve the effective path of the config file the same way
/// `GhostConfig::load` does: honour an explicit `--config <path>` override,
/// otherwise fall back to `<config_dir>/config.toml`.
///
/// Returns `None` only when both no explicit path was given AND
/// `gc_config::config_dir()` couldn't resolve (e.g. `$HOME` unset). In that
/// case there's nothing on disk to read, so we fall through to the
/// defaults-with-banner branch.
fn resolve_config_file_path(config_path: Option<&str>) -> Option<PathBuf> {
    match config_path {
        Some(p) => Some(PathBuf::from(p)),
        None => gc_config::config_dir().map(|d| d.join("config.toml")),
    }
}

/// Print the user's resolved configuration.
///
/// When the config file exists on disk we print it verbatim via
/// `toml_edit::DocumentMut`, which preserves comments, key order, and
/// whitespace — users who carefully annotated their `config.toml` would
/// otherwise lose every comment every time they ran `ghost-complete config`.
///
/// When the file is missing, unreadable, or fails to parse as a
/// round-trippable document, we fall back to serializing the in-memory
/// `GhostConfig` (which is already what `load()` returned, so this reflects
/// the defaults the proxy would actually use) and prepend a banner comment
/// explaining that this is synthesized output rather than the user's file.
pub fn run_config(config_path: Option<&str>) -> Result<()> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    run_config_inner(config_path, &mut handle)
}

pub(crate) fn run_config_inner<W: Write>(config_path: Option<&str>, out: &mut W) -> Result<()> {
    // Load so that bad configs (invalid theme, malformed TOML) still
    // surface an anyhow error consistent with the rest of the CLI —
    // even though the "happy path" output below bypasses the typed
    // representation entirely.
    let config = gc_config::GhostConfig::load(config_path).context("failed to load config")?;

    if let Some(path) = resolve_config_file_path(config_path) {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            match contents.parse::<DocumentMut>() {
                Ok(doc) => {
                    // `toml_edit` preserves comments and string-literal trivia
                    // verbatim. A hostile ~/.config/ghost-complete/config.toml
                    // containing raw ESC/BEL/NUL inside a comment or string
                    // would otherwise round-trip straight to the user's
                    // terminal on `ghost-complete config`. Strip control
                    // bytes at the print boundary while keeping tabs and
                    // newlines so multi-line formatting survives.
                    write!(out, "{}", sanitize_preserving_whitespace(&doc.to_string()))?;
                    return Ok(());
                }
                Err(e) => {
                    // Don't surface as a hard error: `load()` already succeeded
                    // via `toml::from_str`, so the file is valid TOML — the
                    // `toml_edit` parser disagreeing here would be unusual.
                    // Fall through to the defaults branch with a note.
                    tracing::warn!(
                        "toml_edit could not parse {} for verbatim rendering: {e}; falling back to serialized defaults",
                        path.display()
                    );
                }
            }
        }
    }

    // Fallback: no file, unreadable file, or unparseable-by-toml_edit.
    // `toml::to_string_pretty` escapes control bytes in string values
    // (so a `\x1b` in a config value comes out as `\u001b`), but run
    // the output through the same sanitiser anyway as defence in depth.
    let toml_str = toml::to_string_pretty(&config).context("failed to serialize config as TOML")?;
    writeln!(out, "# No config file found; showing defaults.")?;
    write!(out, "{}", sanitize_preserving_whitespace(&toml_str))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dump_sanitizes_hostile_comments_and_strings() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg_path = tmp.path().join("config.toml");

        // TOML strings allow `\uXXXX` escapes that decode to control bytes.
        // A user config could legitimately use them, or a hostile config
        // could embed them to smuggle terminal escape sequences through
        // `ghost-complete config`. toml_edit preserves the raw escape
        // literal in round-trip; writing it as a basic string forces the
        // sanitiser to be the thing that strips ESC/BEL/NUL.
        let body = "[paths]\n\
                    spec_dirs = [\"/tmp/\\u001b[31mbug\\u0007nul\\u0000tail\"]\n";
        std::fs::write(&cfg_path, body).unwrap();

        let mut out = Vec::new();
        run_config_inner(Some(cfg_path.to_str().unwrap()), &mut out).unwrap();
        let emitted = String::from_utf8(out).expect("output must be valid UTF-8");

        assert!(
            !emitted.contains('\x1b'),
            "config dump must not leak raw ESC: {emitted:?}"
        );
        assert!(
            !emitted.contains('\x07'),
            "config dump must not leak raw BEL: {emitted:?}"
        );
        assert!(
            !emitted.contains('\x00'),
            "config dump must not leak raw NUL: {emitted:?}"
        );
    }

    #[test]
    fn config_dump_defaults_branch_sanitizes_control_bytes() {
        // Force the fallback (serialize defaults) branch by pointing at a
        // non-existent config path. Even here, the writer must be routed
        // through `sanitize_preserving_whitespace` — defence in depth.
        let tmp = tempfile::TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist.toml");

        let mut out = Vec::new();
        run_config_inner(Some(missing.to_str().unwrap()), &mut out).unwrap();
        let emitted = String::from_utf8(out).expect("output must be valid UTF-8");

        assert!(
            emitted.starts_with("# No config file found; showing defaults."),
            "fallback banner must be first line, got: {emitted:?}"
        );
        assert!(
            !emitted.contains('\x1b') && !emitted.contains('\x07') && !emitted.contains('\x00'),
            "default dump must not contain any control bytes"
        );
    }
}
