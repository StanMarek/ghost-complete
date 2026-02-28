use std::path::Path;

use anyhow::Result;
use gc_buffer::CommandContext;

use crate::commands::CommandsProvider;
use crate::filesystem::FilesystemProvider;
use crate::fuzzy;
use crate::git;
use crate::history::HistoryProvider;
use crate::provider::Provider;
use crate::specs::{self, SpecStore};
use crate::types::{Suggestion, SuggestionKind};

pub struct SuggestionEngine {
    spec_store: SpecStore,
    filesystem_provider: FilesystemProvider,
    history_provider: HistoryProvider,
    commands_provider: CommandsProvider,
    max_results: usize,
    providers_commands: bool,
    providers_history: bool,
    providers_filesystem: bool,
    providers_specs: bool,
    providers_git: bool,
}

impl SuggestionEngine {
    pub fn new(spec_dir: &Path) -> Result<Self> {
        Ok(Self {
            spec_store: SpecStore::load_from_dir(spec_dir)?,
            filesystem_provider: FilesystemProvider::new(),
            history_provider: HistoryProvider::load(fuzzy::DEFAULT_MAX_RESULTS),
            commands_provider: CommandsProvider::from_path_env(),
            max_results: fuzzy::DEFAULT_MAX_RESULTS,
            providers_commands: true,
            providers_history: true,
            providers_filesystem: true,
            providers_specs: true,
            providers_git: true,
        })
    }

    pub fn with_suggest_config(
        mut self,
        max_results: usize,
        max_history_entries: usize,
        commands: bool,
        history: bool,
        filesystem: bool,
        specs: bool,
        git: bool,
    ) -> Self {
        self.max_results = max_results;
        self.providers_commands = commands;
        self.providers_history = history;
        self.providers_filesystem = filesystem;
        self.providers_specs = specs;
        self.providers_git = git;
        // Reload history with new max
        self.history_provider = HistoryProvider::load(max_history_entries);
        self
    }

    #[cfg(test)]
    fn with_providers(
        spec_store: SpecStore,
        history_provider: HistoryProvider,
        commands_provider: CommandsProvider,
    ) -> Self {
        Self {
            spec_store,
            filesystem_provider: FilesystemProvider::new(),
            history_provider,
            commands_provider,
            max_results: fuzzy::DEFAULT_MAX_RESULTS,
            providers_commands: true,
            providers_history: true,
            providers_filesystem: true,
            providers_specs: true,
            providers_git: true,
        }
    }

    pub fn suggest_sync(&self, ctx: &CommandContext, cwd: &Path) -> Result<Vec<Suggestion>> {
        let mut candidates = Vec::new();

        // Command position: commands + history
        if ctx.word_index == 0 {
            if self.providers_commands {
                if let Ok(cmds) = self.commands_provider.provide(ctx, cwd) {
                    candidates.extend(cmds);
                }
            }
            if self.providers_history {
                if let Ok(hist) = self.history_provider.provide(ctx, cwd) {
                    candidates.extend(hist);
                }
            }
            return Ok(fuzzy::rank(&ctx.current_word, candidates, self.max_results));
        }

        // Redirect: always filesystem
        if ctx.in_redirect {
            if self.providers_filesystem {
                if let Ok(fs) = self.filesystem_provider.provide(ctx, cwd) {
                    candidates.extend(fs);
                }
            }
            return Ok(fuzzy::rank(&ctx.current_word, candidates, self.max_results));
        }

        // Path-like current_word: filesystem
        if looks_like_path(&ctx.current_word) {
            if self.providers_filesystem {
                if let Ok(fs) = self.filesystem_provider.provide(ctx, cwd) {
                    candidates.extend(fs);
                }
            }
            return Ok(fuzzy::rank(&ctx.current_word, candidates, self.max_results));
        }

        // Check for a spec for this command
        if self.providers_specs {
            if let Some(command) = &ctx.command {
                if let Some(spec) = self.spec_store.get(command) {
                    let resolution = specs::resolve_spec(spec, ctx);

                    // Add subcommands and options from the spec
                    candidates.extend(resolution.subcommands);
                    candidates.extend(resolution.options);

                    // Handle generators (e.g., git branches/tags/remotes)
                    if self.providers_git {
                        for gen_type in &resolution.generators {
                            if let Some(kind) = git::generator_to_query_kind(gen_type) {
                                if let Ok(git_suggestions) = git::git_suggestions(cwd, kind) {
                                    candidates.extend(git_suggestions);
                                }
                            }
                        }
                    }

                    // Add filesystem: folders-only or all filepaths
                    if resolution.wants_folders_only && self.providers_filesystem {
                        if let Ok(fs) = self.filesystem_provider.provide(ctx, cwd) {
                            candidates.extend(
                                fs.into_iter()
                                    .filter(|s| s.kind == SuggestionKind::Directory),
                            );
                        }
                    } else if resolution.wants_filepaths && self.providers_filesystem {
                        if let Ok(fs) = self.filesystem_provider.provide(ctx, cwd) {
                            candidates.extend(fs);
                        }
                    }

                    return Ok(fuzzy::rank(&ctx.current_word, candidates, self.max_results));
                }
            }
        }

        // No spec — fallback to filesystem
        if self.providers_filesystem {
            if let Ok(fs) = self.filesystem_provider.provide(ctx, cwd) {
                candidates.extend(fs);
            }
        }
        Ok(fuzzy::rank(&ctx.current_word, candidates, self.max_results))
    }
}

fn looks_like_path(word: &str) -> bool {
    word.contains('/') || word.starts_with('.') || word.starts_with('~')
}

#[cfg(test)]
mod tests {
    use super::*;
    use gc_buffer::QuoteState;
    use std::path::PathBuf;

    fn spec_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs")
    }

    fn make_engine() -> SuggestionEngine {
        let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap();
        let history = HistoryProvider::from_entries(vec![
            "git push".into(),
            "cargo build".into(),
            "ls -la".into(),
        ]);
        let commands = CommandsProvider::from_list(vec!["git".into(), "ls".into(), "cargo".into()]);
        SuggestionEngine::with_providers(spec_store, history, commands)
    }

    fn make_ctx(
        command: Option<&str>,
        args: Vec<&str>,
        current_word: &str,
        word_index: usize,
    ) -> CommandContext {
        CommandContext {
            command: command.map(String::from),
            args: args.into_iter().map(String::from).collect(),
            current_word: current_word.to_string(),
            word_index,
            is_flag: current_word.starts_with('-'),
            is_long_flag: current_word.starts_with("--"),
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: QuoteState::None,
        }
    }

    #[test]
    fn test_command_position_returns_commands_and_history() {
        let engine = make_engine();
        let ctx = make_ctx(None, vec![], "gi", 0);
        let results = engine.suggest_sync(&ctx, Path::new("/tmp")).unwrap();
        // Should have "git" from both commands and history
        assert!(results.iter().any(|s| s.text == "git"));
    }

    #[test]
    fn test_spec_subcommands() {
        let engine = make_engine();
        let ctx = make_ctx(Some("git"), vec![], "ch", 1);
        let results = engine.suggest_sync(&ctx, Path::new("/tmp")).unwrap();
        assert!(
            results.iter().any(|s| s.text == "checkout"),
            "expected 'checkout' in results: {results:?}"
        );
    }

    #[test]
    fn test_spec_options() {
        let engine = make_engine();
        // Query "--" should match long flags like --message, --amend, etc.
        let ctx = make_ctx(Some("git"), vec!["commit"], "--", 2);
        let results = engine.suggest_sync(&ctx, Path::new("/tmp")).unwrap();
        assert!(
            results.iter().any(|s| s.text == "--message"),
            "expected '--message' in results: {results:?}"
        );
        assert!(
            results.iter().any(|s| s.text == "--amend"),
            "expected '--amend' in results: {results:?}"
        );

        // Query "-" should match short flags like -m, -a
        let ctx = make_ctx(Some("git"), vec!["commit"], "-", 2);
        let results = engine.suggest_sync(&ctx, Path::new("/tmp")).unwrap();
        assert!(
            results.iter().any(|s| s.text == "-m"),
            "expected '-m' in results: {results:?}"
        );
    }

    #[test]
    fn test_redirect_gives_filesystem() {
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("output.txt"), "").unwrap();
        let mut ctx = make_ctx(Some("echo"), vec!["hello"], "", 2);
        ctx.in_redirect = true;
        let results = engine.suggest_sync(&ctx, tmp.path()).unwrap();
        assert!(results.iter().any(|s| s.text == "output.txt"));
    }

    #[test]
    fn test_path_prefix_triggers_filesystem() {
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "").unwrap();
        let ctx = make_ctx(Some("cat"), vec![], "src/", 1);
        let results = engine.suggest_sync(&ctx, tmp.path()).unwrap();
        assert!(
            results.iter().any(|s| s.text == "src/main.rs"),
            "expected 'src/main.rs' in results: {results:?}"
        );
    }

    #[test]
    fn test_unknown_command_falls_back_to_filesystem() {
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("data.csv"), "").unwrap();
        let ctx = make_ctx(Some("unknown_cmd"), vec![], "", 1);
        let results = engine.suggest_sync(&ctx, tmp.path()).unwrap();
        assert!(results.iter().any(|s| s.text == "data.csv"));
    }

    #[test]
    fn test_empty_results_for_no_matches() {
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = make_ctx(Some("git"), vec![], "zzzzzzz_no_match", 1);
        let results = engine.suggest_sync(&ctx, tmp.path()).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_cd_only_shows_directories() {
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("mydir")).unwrap();
        std::fs::write(tmp.path().join("myfile.txt"), "").unwrap();
        let ctx = make_ctx(Some("cd"), vec![], "", 1);
        let results = engine.suggest_sync(&ctx, tmp.path()).unwrap();
        assert!(
            results.iter().any(|s| s.text.contains("mydir")),
            "cd should show directories: {results:?}"
        );
        assert!(
            !results.iter().any(|s| s.text.contains("myfile")),
            "cd should NOT show files: {results:?}"
        );
    }

    #[test]
    fn test_disabled_commands_provider() {
        let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap();
        let history = HistoryProvider::from_entries(vec![]);
        let commands = CommandsProvider::from_list(vec!["git".into(), "ls".into()]);
        let engine = SuggestionEngine::with_providers(spec_store, history, commands)
            .with_suggest_config(50, 10_000, false, true, true, true, true);

        let ctx = make_ctx(None, vec![], "gi", 0);
        let results = engine.suggest_sync(&ctx, Path::new("/tmp")).unwrap();
        // Commands provider disabled — should not find "git" from commands
        assert!(
            !results.iter().any(|s| s.source == crate::types::SuggestionSource::Commands),
            "should not have commands when provider disabled"
        );
    }
}
