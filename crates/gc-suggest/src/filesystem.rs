use std::path::{Path, PathBuf};

use anyhow::Result;
use gc_buffer::CommandContext;

use crate::provider::Provider;
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

pub struct FilesystemProvider;

impl FilesystemProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Provider for FilesystemProvider {
    fn provide(&self, ctx: &CommandContext, cwd: &Path) -> Result<Vec<Suggestion>> {
        let (dir, prefix) = resolve_path(&ctx.current_word, cwd);
        list_entries(&dir, &prefix, &ctx.current_word)
    }

    fn name(&self) -> &'static str {
        "filesystem"
    }
}

/// Resolve the current_word into a directory to list and a display prefix.
///
/// - Empty word → (cwd, "")
/// - "src/" → (cwd/src, "src/")
/// - "~/Documents/" → (~home/Documents, "~/Documents/")
/// - "/usr/bin/" → (/usr/bin, "/usr/bin/")
/// - "src/ma" → (cwd/src, "src/")
fn resolve_path(current_word: &str, cwd: &Path) -> (PathBuf, String) {
    if current_word.is_empty() {
        return (cwd.to_path_buf(), String::new());
    }

    let expanded = if let Some(rest) = current_word.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            home.join(rest).to_string_lossy().to_string()
        } else {
            current_word.to_string()
        }
    } else {
        current_word.to_string()
    };

    let path = Path::new(&expanded);
    let abs_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };

    // If current_word ends with '/', list that directory with full prefix
    if current_word.ends_with('/') {
        return (abs_path, current_word.to_string());
    }

    // Split off the partial filename — list the parent dir
    // If no '/' in current_word, the partial is just a filename in CWD
    if current_word.contains('/') {
        if let Some(parent) = abs_path.parent() {
            let idx = current_word.rfind('/').unwrap();
            let prefix = format!("{}/", &current_word[..idx]);
            (parent.to_path_buf(), prefix)
        } else {
            (abs_path, current_word.to_string())
        }
    } else {
        // No slash — list CWD, partial is the current_word itself
        (cwd.to_path_buf(), String::new())
    }
}

fn list_entries(dir: &Path, prefix: &str, current_word: &str) -> Result<Vec<Suggestion>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(Vec::new()),
    };

    // Determine if we should show hidden files: only if the partial filename
    // (the part after the last '/') starts with '.'
    let partial = if let Some(idx) = current_word.rfind('/') {
        &current_word[idx + 1..]
    } else {
        current_word
    };
    let show_hidden = partial.starts_with('.');

    let mut suggestions = Vec::new();

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip hidden files unless explicitly typing a dot
        if name_str.starts_with('.') && !show_hidden {
            continue;
        }

        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);

        let display = if is_dir {
            format!("{prefix}{name_str}/")
        } else {
            format!("{prefix}{name_str}")
        };

        let kind = if is_dir {
            SuggestionKind::Directory
        } else {
            SuggestionKind::FilePath
        };

        suggestions.push(Suggestion {
            text: display,
            description: None,
            kind,
            source: SuggestionSource::Filesystem,
            score: 0,
        });
    }

    suggestions.sort_by(|a, b| a.text.cmp(&b.text));
    Ok(suggestions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gc_buffer::QuoteState;
    use tempfile::TempDir;

    fn ctx_with_word(word: &str) -> CommandContext {
        CommandContext {
            command: Some("ls".into()),
            args: vec![],
            current_word: word.to_string(),
            word_index: 1,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: QuoteState::None,
        }
    }

    fn setup_dir() -> TempDir {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("file1.txt"), "").unwrap();
        std::fs::write(tmp.path().join("file2.rs"), "").unwrap();
        std::fs::create_dir(tmp.path().join("mydir")).unwrap();
        std::fs::write(tmp.path().join("mydir/nested.txt"), "").unwrap();
        std::fs::write(tmp.path().join(".hidden"), "").unwrap();
        tmp
    }

    #[test]
    fn test_list_cwd() {
        let tmp = setup_dir();
        let ctx = ctx_with_word("");
        let provider = FilesystemProvider::new();
        let results = provider.provide(&ctx, tmp.path()).unwrap();
        // Should list file1.txt, file2.rs, mydir/ (not .hidden)
        assert_eq!(results.len(), 3);
        assert!(results.iter().any(|s| s.text == "file1.txt"));
        assert!(results.iter().any(|s| s.text == "file2.rs"));
        assert!(results.iter().any(|s| s.text == "mydir/"));
    }

    #[test]
    fn test_list_subdirectory() {
        let tmp = setup_dir();
        let ctx = ctx_with_word("mydir/");
        let provider = FilesystemProvider::new();
        let results = provider.provide(&ctx, tmp.path()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].text, "mydir/nested.txt");
    }

    #[test]
    fn test_hidden_files_excluded_by_default() {
        let tmp = setup_dir();
        let ctx = ctx_with_word("");
        let provider = FilesystemProvider::new();
        let results = provider.provide(&ctx, tmp.path()).unwrap();
        assert!(!results.iter().any(|s| s.text.contains(".hidden")));
    }

    #[test]
    fn test_hidden_files_included_with_dot() {
        let tmp = setup_dir();
        let ctx = ctx_with_word(".");
        let provider = FilesystemProvider::new();
        let results = provider.provide(&ctx, tmp.path()).unwrap();
        assert!(results.iter().any(|s| s.text == ".hidden"));
    }

    #[test]
    fn test_nonexistent_dir_returns_empty() {
        let tmp = setup_dir();
        let ctx = ctx_with_word("nonexistent/");
        let provider = FilesystemProvider::new();
        let results = provider.provide(&ctx, tmp.path()).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_directories_have_trailing_slash() {
        let tmp = setup_dir();
        let ctx = ctx_with_word("");
        let provider = FilesystemProvider::new();
        let results = provider.provide(&ctx, tmp.path()).unwrap();
        let dirs: Vec<&Suggestion> = results
            .iter()
            .filter(|s| s.kind == SuggestionKind::Directory)
            .collect();
        assert!(!dirs.is_empty());
        for d in dirs {
            assert!(d.text.ends_with('/'), "directory should end with /");
        }
    }
}
