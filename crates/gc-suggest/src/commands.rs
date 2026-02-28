use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use anyhow::Result;
use gc_buffer::CommandContext;

use crate::provider::Provider;
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

pub struct CommandsProvider {
    commands: Vec<String>,
}

impl CommandsProvider {
    pub fn from_path_env() -> Self {
        let commands = match Self::scan_path() {
            Ok(cmds) => cmds,
            Err(e) => {
                tracing::debug!("failed to scan $PATH: {e}");
                Vec::new()
            }
        };
        Self { commands }
    }

    /// Test constructor — inject command list directly.
    #[cfg(test)]
    pub fn from_list(commands: Vec<String>) -> Self {
        Self { commands }
    }

    fn scan_path() -> Result<Vec<String>> {
        let path_var = std::env::var("PATH")?;
        let mut seen = HashSet::new();
        let mut commands = Vec::new();

        for dir in path_var.split(':') {
            let entries = match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(_) => continue,
            };

            for entry in entries {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                // Skip non-executable files
                let metadata = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if metadata.is_dir() {
                    continue;
                }
                if metadata.permissions().mode() & 0o111 == 0 {
                    continue;
                }

                let name = entry.file_name();
                let name_str = name.to_string_lossy().to_string();

                if seen.insert(name_str.clone()) {
                    commands.push(name_str);
                }
            }
        }

        commands.sort();
        Ok(commands)
    }
}

impl Provider for CommandsProvider {
    fn provide(&self, ctx: &CommandContext, _cwd: &Path) -> Result<Vec<Suggestion>> {
        // Only provide at command position
        if ctx.word_index != 0 {
            return Ok(Vec::new());
        }

        let suggestions = self
            .commands
            .iter()
            .map(|cmd| Suggestion {
                text: cmd.clone(),
                description: None,
                kind: SuggestionKind::Command,
                source: SuggestionSource::Commands,
                score: 0,
            })
            .collect();

        Ok(suggestions)
    }

    fn name(&self) -> &'static str {
        "commands"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gc_buffer::QuoteState;

    fn cmd_position_ctx(word: &str) -> CommandContext {
        CommandContext {
            command: None,
            args: vec![],
            current_word: word.to_string(),
            word_index: 0,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: QuoteState::None,
        }
    }

    fn arg_position_ctx() -> CommandContext {
        CommandContext {
            command: Some("git".into()),
            args: vec![],
            current_word: String::new(),
            word_index: 1,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: QuoteState::None,
        }
    }

    #[test]
    fn test_provides_at_command_position() {
        let provider = CommandsProvider::from_list(vec!["git".into(), "ls".into(), "cargo".into()]);
        let ctx = cmd_position_ctx("gi");
        let results = provider.provide(&ctx, Path::new("/tmp")).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_empty_at_arg_position() {
        let provider = CommandsProvider::from_list(vec!["git".into(), "ls".into()]);
        let ctx = arg_position_ctx();
        let results = provider.provide(&ctx, Path::new("/tmp")).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_scan_path_does_not_panic() {
        // Just ensure scan doesn't crash
        let _ = CommandsProvider::from_path_env();
    }

    #[test]
    fn test_commands_are_sorted() {
        let provider = CommandsProvider::from_path_env();
        for window in provider.commands.windows(2) {
            assert!(
                window[0] <= window[1],
                "commands should be sorted: {} > {}",
                window[0],
                window[1]
            );
        }
    }

    #[test]
    fn test_no_duplicates() {
        let provider = CommandsProvider::from_path_env();
        let mut seen = HashSet::new();
        for cmd in &provider.commands {
            assert!(seen.insert(cmd), "duplicate command: {cmd}");
        }
    }
}
