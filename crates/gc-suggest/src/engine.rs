use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gc_buffer::CommandContext;
use tokio::sync::Semaphore;

use crate::cache::{CacheKey, GeneratorCache};
use crate::commands::CommandsProvider;
use crate::filesystem::FilesystemProvider;
use crate::fuzzy;
use crate::git;
use crate::history::{HistoryProvider, DEFAULT_MAX_HISTORY_ENTRIES};
use crate::provider::Provider;
use crate::script::{run_script, substitute_template};
use crate::specs::{self, GeneratorSpec, SpecStore};
use crate::transform::execute_pipeline;
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

/// Maximum number of concurrent script generators.
const MAX_CONCURRENT_GENERATORS: usize = 3;

pub struct SuggestionEngine {
    spec_store: SpecStore,
    filesystem_provider: FilesystemProvider,
    history_provider: HistoryProvider,
    commands_provider: CommandsProvider,
    generator_cache: Arc<GeneratorCache>,
    max_results: usize,
    max_history_results: usize,
    providers_commands: bool,
    providers_filesystem: bool,
    providers_specs: bool,
    providers_git: bool,
}

impl SuggestionEngine {
    pub fn new(spec_dir: &Path) -> Result<Self> {
        let result = SpecStore::load_from_dir(spec_dir)?;
        if !result.errors.is_empty() {
            tracing::warn!(
                "{} spec(s) failed to load (run `ghost-complete validate-specs` for details): {}",
                result.errors.len(),
                result.errors.join(", ")
            );
        }
        Ok(Self {
            spec_store: result.store,
            filesystem_provider: FilesystemProvider::new(),
            history_provider: HistoryProvider::load(DEFAULT_MAX_HISTORY_ENTRIES),
            commands_provider: CommandsProvider::from_path_env(),
            generator_cache: Arc::new(GeneratorCache::new()),
            max_results: fuzzy::DEFAULT_MAX_RESULTS,
            max_history_results: 5,
            providers_commands: true,
            providers_filesystem: true,
            providers_specs: true,
            providers_git: true,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_suggest_config(
        mut self,
        max_results: usize,
        max_history_entries: usize,
        commands: bool,
        max_history_results: usize,
        filesystem: bool,
        specs: bool,
        git: bool,
    ) -> Self {
        self.max_results = max_results;
        self.max_history_results = max_history_results;
        self.providers_commands = commands;
        self.providers_filesystem = filesystem;
        self.providers_specs = specs;
        self.providers_git = git;
        // Reload history only if enabled
        if max_history_results > 0 {
            self.history_provider = HistoryProvider::load(max_history_entries);
        } else {
            self.history_provider = HistoryProvider::from_entries(vec![]);
        }
        self
    }

    /// Test/bench constructor — inject providers directly for deterministic setup.
    pub fn with_providers(
        spec_store: SpecStore,
        history_provider: HistoryProvider,
        commands_provider: CommandsProvider,
    ) -> Self {
        Self {
            spec_store,
            filesystem_provider: FilesystemProvider::new(),
            history_provider,
            commands_provider,
            generator_cache: Arc::new(GeneratorCache::new()),
            max_results: fuzzy::DEFAULT_MAX_RESULTS,
            max_history_results: 5,
            providers_commands: true,
            providers_filesystem: true,
            providers_specs: true,
            providers_git: true,
        }
    }

    /// Test helper — set the history results cap without reloading from disk.
    #[cfg(test)]
    pub fn with_max_history_results(mut self, n: usize) -> Self {
        self.max_history_results = n;
        self
    }

    /// Quick check: does the current command context have any script generators?
    /// Used to avoid spawning async tasks when there's nothing to run.
    pub fn has_script_generators(&self, ctx: &CommandContext) -> bool {
        if !self.providers_specs || ctx.word_index == 0 || ctx.in_redirect {
            return false;
        }
        let command = match &ctx.command {
            Some(c) => c,
            None => return false,
        };
        let spec = match self.spec_store.get(command) {
            Some(s) => s,
            None => return false,
        };
        let resolution = specs::resolve_spec(spec, ctx);
        resolution.script_generators.iter().any(|g| !g.requires_js)
    }

    /// Run script-based generators for the current command context.
    ///
    /// Resolves the spec, finds script generators, runs each (with caching and
    /// concurrency limiting), applies transform pipelines, and returns all results.
    pub async fn suggest_dynamic(
        &self,
        ctx: &CommandContext,
        cwd: &Path,
        timeout_ms: u64,
    ) -> Result<Vec<Suggestion>> {
        if !self.providers_specs || ctx.word_index == 0 || ctx.in_redirect {
            return Ok(Vec::new());
        }

        let command = match &ctx.command {
            Some(c) => c,
            None => return Ok(Vec::new()),
        };

        let spec = match self.spec_store.get(command) {
            Some(s) => s,
            None => return Ok(Vec::new()),
        };

        let resolution = specs::resolve_spec(spec, ctx);
        if resolution.script_generators.is_empty() {
            return Ok(Vec::new());
        }

        let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_GENERATORS));
        let mut handles = Vec::new();

        for gen in &resolution.script_generators {
            if gen.requires_js {
                continue;
            }

            let argv = resolve_script_argv(gen, ctx);
            if argv.is_empty() {
                continue;
            }

            // Check cache
            let cache_cwd = gen
                .cache
                .as_ref()
                .filter(|c| c.cache_by_directory)
                .map(|_| cwd);
            let cache_key = CacheKey::from_strings(command, &argv, cache_cwd);

            if let Some(cached) = self.generator_cache.get(&cache_key) {
                tracing::debug!("cache hit for generator {:?}", argv);
                handles.push(tokio::spawn(async move { Ok::<_, anyhow::Error>(cached) }));
                continue;
            }

            let permit = Arc::clone(&semaphore);
            let cwd = cwd.to_path_buf();
            let transforms = gen.transforms.clone();
            let cache = gen.cache.clone();
            let cache_store = Arc::clone(&self.generator_cache);
            let cmd_name = command.to_string();

            handles.push(tokio::spawn(async move {
                let _permit = permit
                    .acquire()
                    .await
                    .map_err(|e| anyhow::anyhow!("semaphore error: {e}"))?;

                let argv_refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
                let output = run_script(&argv_refs, &cwd, timeout_ms).await?;

                let suggestions = if transforms.is_empty() {
                    // Default: split on newlines, filter empty, produce plain suggestions
                    output
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .map(|l| Suggestion {
                            text: l.to_string(),
                            source: SuggestionSource::Script,
                            ..Default::default()
                        })
                        .collect()
                } else {
                    execute_pipeline(&output, &transforms).map_err(|e| anyhow::anyhow!("{e}"))?
                };

                // Cache if configured
                if let Some(ref cache_cfg) = cache {
                    if cache_cfg.ttl_seconds > 0 {
                        let cache_cwd = if cache_cfg.cache_by_directory {
                            Some(cwd.as_path())
                        } else {
                            None
                        };
                        let key = CacheKey::from_strings(&cmd_name, &argv, cache_cwd);
                        cache_store.insert(
                            key,
                            suggestions.clone(),
                            Duration::from_secs(cache_cfg.ttl_seconds),
                        );
                    }
                }

                Ok(suggestions)
            }));
        }

        let mut all_results = Vec::new();
        for handle in handles {
            match handle.await {
                Ok(Ok(suggestions)) => all_results.extend(suggestions),
                Ok(Err(e)) => {
                    tracing::warn!("script generator failed: {e}");
                }
                Err(e) => {
                    tracing::warn!("script generator task panicked: {e}");
                }
            }
        }

        Ok(fuzzy::rank(
            &ctx.current_word,
            all_results,
            self.max_results,
        ))
    }

    pub fn suggest_sync(
        &self,
        ctx: &CommandContext,
        cwd: &Path,
        buffer: &str,
    ) -> Result<Vec<Suggestion>> {
        let mut candidates = Vec::new();

        // Command position: commands (history handled by rank_with_history)
        if ctx.word_index == 0 {
            if self.providers_commands {
                match self.commands_provider.provide(ctx, cwd) {
                    Ok(cmds) => candidates.extend(cmds),
                    Err(e) => tracing::debug!("commands provider error: {e}"),
                }
            }
            return Ok(self.rank_with_history(ctx, cwd, buffer, candidates));
        }

        // Redirect: always filesystem
        if ctx.in_redirect {
            if self.providers_filesystem {
                match self.filesystem_provider.provide(ctx, cwd) {
                    Ok(fs) => candidates.extend(fs),
                    Err(e) => tracing::debug!("filesystem provider error (redirect): {e}"),
                }
            }
            return Ok(self.rank_with_history(ctx, cwd, buffer, candidates));
        }

        // Check for a spec for this command (before path heuristic — specs
        // know about folders-only vs all-filepaths and should take priority)
        if self.providers_specs {
            if let Some(command) = &ctx.command {
                if let Some(spec) = self.spec_store.get(command) {
                    let resolution = specs::resolve_spec(spec, ctx);

                    // When the preceding flag takes an argument (templates or
                    // generators are set from the option's args), show ONLY
                    // those arg completions — not the full subcommand/option
                    // list.  The user typed e.g. `curl -o ` and wants files,
                    // not more flags.
                    let in_option_arg = ctx.preceding_flag.is_some()
                        && (resolution.wants_filepaths
                            || resolution.wants_folders_only
                            || !resolution.native_generators.is_empty());

                    if !in_option_arg {
                        candidates.extend(resolution.subcommands);
                        candidates.extend(resolution.options);
                    }

                    // Handle generators (e.g., git branches/tags/remotes)
                    if self.providers_git {
                        for gen_type in &resolution.native_generators {
                            if let Some(kind) = git::generator_to_query_kind(gen_type) {
                                match git::git_suggestions(cwd, kind) {
                                    Ok(suggestions) => candidates.extend(suggestions),
                                    Err(e) => {
                                        tracing::debug!("git provider error ({gen_type}): {e}")
                                    }
                                }
                            }
                        }
                    }

                    // Add filesystem: folders-only or all filepaths
                    if resolution.wants_folders_only && self.providers_filesystem {
                        // Offer "../" to navigate up, unless at / or $HOME
                        let parent_text = if ctx.current_word.is_empty() {
                            Some("../".to_string())
                        } else if ctx.current_word.ends_with("../") {
                            Some(format!("{}../", ctx.current_word))
                        } else {
                            None
                        };
                        if let Some(text) = parent_text {
                            let effective = cwd.join(&ctx.current_word);
                            let at_boundary =
                                effective.canonicalize().ok().map_or(true, |resolved| {
                                    resolved == Path::new("/")
                                        || std::env::var("HOME")
                                            .ok()
                                            .is_some_and(|h| resolved == Path::new(&h))
                                });
                            if !at_boundary {
                                candidates.push(Suggestion {
                                    text,
                                    description: Some("Parent directory".to_string()),
                                    kind: SuggestionKind::Directory,
                                    source: SuggestionSource::Filesystem,
                                    ..Default::default()
                                });
                            }
                        }
                        match self.filesystem_provider.provide(ctx, cwd) {
                            Ok(fs) => {
                                candidates.extend(
                                    fs.into_iter()
                                        .filter(|s| s.kind == SuggestionKind::Directory),
                                );
                            }
                            Err(e) => tracing::debug!("filesystem provider error (folders): {e}"),
                        }
                    } else if resolution.wants_filepaths && self.providers_filesystem {
                        match self.filesystem_provider.provide(ctx, cwd) {
                            Ok(fs) => candidates.extend(fs),
                            Err(e) => tracing::debug!("filesystem provider error: {e}"),
                        }
                    }

                    return Ok(self.rank_with_history(ctx, cwd, buffer, candidates));
                }
            }
        }

        // Path-like current_word without a spec: filesystem
        if looks_like_path(&ctx.current_word) {
            if self.providers_filesystem {
                match self.filesystem_provider.provide(ctx, cwd) {
                    Ok(fs) => candidates.extend(fs),
                    Err(e) => tracing::debug!("filesystem provider error (path): {e}"),
                }
            }
            return Ok(self.rank_with_history(ctx, cwd, buffer, candidates));
        }

        // No spec, no path — fallback to filesystem
        if self.providers_filesystem {
            match self.filesystem_provider.provide(ctx, cwd) {
                Ok(fs) => candidates.extend(fs),
                Err(e) => tracing::debug!("filesystem provider error (fallback): {e}"),
            }
        }
        Ok(self.rank_with_history(ctx, cwd, buffer, candidates))
    }

    /// Rank main candidates with current_word, then separately rank history
    /// candidates with the full buffer, and append history results at the end.
    fn rank_with_history(
        &self,
        ctx: &CommandContext,
        cwd: &Path,
        buffer: &str,
        candidates: Vec<Suggestion>,
    ) -> Vec<Suggestion> {
        let mut results = fuzzy::rank(&ctx.current_word, candidates, self.max_results);

        // History doesn't belong in redirect context — user expects filenames, not commands
        if self.max_history_results > 0 && !ctx.in_redirect {
            let remaining = self
                .max_history_results
                .min(self.max_results.saturating_sub(results.len()));
            if remaining > 0 {
                match self.history_provider.provide(ctx, cwd) {
                    Ok(hist) if !hist.is_empty() => {
                        let hist_results = fuzzy::rank(buffer, hist, remaining);
                        results.extend(hist_results);
                    }
                    Ok(_) => {}
                    Err(e) => tracing::debug!("history provider error: {e}"),
                }
            }
        }

        results
    }
}

/// Resolve the argv for a script generator, applying template substitution if needed.
fn resolve_script_argv(gen: &GeneratorSpec, ctx: &CommandContext) -> Vec<String> {
    if let Some(ref script) = gen.script {
        return script.clone();
    }
    if let Some(ref template) = gen.script_template {
        let prev_token = ctx.args.last().map(|s| s.as_str());
        let current_token = if ctx.current_word.is_empty() {
            None
        } else {
            Some(ctx.current_word.as_str())
        };
        return substitute_template(template, prev_token, current_token);
    }
    Vec::new()
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
        let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap().store;
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
            is_first_segment: true,
        }
    }

    #[test]
    fn test_command_position_returns_commands_and_history() {
        let engine = make_engine();
        let ctx = make_ctx(None, vec![], "gi", 0);
        let results = engine.suggest_sync(&ctx, Path::new("/tmp"), "gi").unwrap();
        // Should have "git" from both commands and history
        assert!(results.iter().any(|s| s.text == "git"));
    }

    #[test]
    fn test_spec_subcommands() {
        let engine = make_engine();
        let ctx = make_ctx(Some("git"), vec![], "ch", 1);
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "git ch")
            .unwrap();
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
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "git commit --")
            .unwrap();
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
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "git commit -")
            .unwrap();
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
        let results = engine
            .suggest_sync(&ctx, tmp.path(), "echo hello ")
            .unwrap();
        assert!(results.iter().any(|s| s.text == "output.txt"));
    }

    #[test]
    fn test_path_prefix_triggers_filesystem() {
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "").unwrap();
        let ctx = make_ctx(Some("cat"), vec![], "src/", 1);
        let results = engine.suggest_sync(&ctx, tmp.path(), "cat src/").unwrap();
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
        let results = engine
            .suggest_sync(&ctx, tmp.path(), "unknown_cmd_xyz ")
            .unwrap();
        assert!(results.iter().any(|s| s.text == "data.csv"));
    }

    #[test]
    fn test_empty_results_for_no_matches() {
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = make_ctx(Some("git"), vec![], "zzzzzzz_no_match", 1);
        let results = engine
            .suggest_sync(&ctx, tmp.path(), "git zzzzzzz_no_match")
            .unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_cd_only_shows_directories() {
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("mydir")).unwrap();
        std::fs::write(tmp.path().join("myfile.txt"), "").unwrap();
        let ctx = make_ctx(Some("cd"), vec![], "", 1);
        let results = engine.suggest_sync(&ctx, tmp.path(), "cd ").unwrap();
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
    fn test_option_arg_template_triggers_filesystem() {
        // pip install -r <TAB> → should show files from the filesystem
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("requirements.txt"), "").unwrap();
        std::fs::write(tmp.path().join("setup.py"), "").unwrap();

        let ctx = CommandContext {
            command: Some("pip".into()),
            args: vec!["install".into(), "-r".into()],
            current_word: String::new(),
            word_index: 3,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: Some("-r".into()),
            in_pipe: false,
            in_redirect: false,
            quote_state: QuoteState::None,
            is_first_segment: true,
        };
        let results = engine
            .suggest_sync(&ctx, tmp.path(), "pip install -r ")
            .unwrap();
        assert!(
            results.iter().any(|s| s.text == "requirements.txt"),
            "pip install -r should show files: {results:?}"
        );
    }

    #[test]
    fn test_curl_dash_o_shows_files_from_real_spec() {
        // Uses the ACTUAL curl.json spec from disk — not a synthetic one
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("output.html"), "").unwrap();
        std::fs::write(tmp.path().join("data.json"), "").unwrap();

        // Simulate: curl -o <TAB>
        let ctx = CommandContext {
            command: Some("curl".into()),
            args: vec!["-o".into()],
            current_word: String::new(),
            word_index: 2,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: Some("-o".into()),
            in_pipe: false,
            in_redirect: false,
            quote_state: QuoteState::None,
            is_first_segment: true,
        };
        let results = engine.suggest_sync(&ctx, tmp.path(), "curl -o ").unwrap();

        let file_results: Vec<_> = results
            .iter()
            .filter(|s| s.source == crate::types::SuggestionSource::Filesystem)
            .collect();

        eprintln!(
            "All results for curl -o: {:?}",
            results
                .iter()
                .map(|s| (&s.text, &s.source, &s.kind))
                .collect::<Vec<_>>()
        );
        eprintln!(
            "File results: {:?}",
            file_results.iter().map(|s| &s.text).collect::<Vec<_>>()
        );

        assert!(
            !file_results.is_empty(),
            "curl -o should show filesystem results, got: {results:?}"
        );
    }

    #[test]
    fn test_option_arg_folders_template_filters_files() {
        // test-deploy -t <TAB> → should show only directories
        // Uses an inline spec to avoid dependency on real specs
        let spec_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            spec_dir.path().join("test-deploy.json"),
            r#"{"name":"test-deploy","subcommands":[{"name":"install","options":[{"name":["-t","--target"],"description":"Target directory","args":{"name":"dir","template":"folders"}}]}]}"#,
        )
        .unwrap();
        let spec_store = SpecStore::load_from_dir(spec_dir.path()).unwrap().store;
        let history = HistoryProvider::from_entries(vec![]);
        let commands = CommandsProvider::from_list(vec![]);
        let engine = SuggestionEngine::with_providers(spec_store, history, commands);

        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("target_dir")).unwrap();
        std::fs::write(tmp.path().join("not_a_dir.txt"), "").unwrap();

        let ctx = CommandContext {
            command: Some("test-deploy".into()),
            args: vec!["install".into(), "-t".into()],
            current_word: String::new(),
            word_index: 3,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: Some("-t".into()),
            in_pipe: false,
            in_redirect: false,
            quote_state: QuoteState::None,
            is_first_segment: true,
        };
        let results = engine
            .suggest_sync(&ctx, tmp.path(), "test-deploy install -t ")
            .unwrap();
        assert!(
            results.iter().any(|s| s.text.contains("target_dir")),
            "test-deploy install -t should show directories: {results:?}"
        );
        assert!(
            !results.iter().any(|s| s.text.contains("not_a_dir")),
            "test-deploy install -t should NOT show files: {results:?}"
        );
    }

    #[test]
    fn test_cd_first_suggestion_is_parent_dir() {
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("aaa")).unwrap();
        std::fs::create_dir(tmp.path().join("bbb")).unwrap();
        let ctx = make_ctx(Some("cd"), vec![], "", 1);
        let results = engine.suggest_sync(&ctx, tmp.path(), "cd ").unwrap();
        assert!(!results.is_empty(), "cd should return suggestions");
        assert_eq!(
            results[0].text, "../",
            "first cd suggestion should be ../, got: {:?}",
            results[0].text
        );
    }

    #[test]
    fn test_cd_parent_dir_absent_at_root() {
        let engine = make_engine();
        let ctx = make_ctx(Some("cd"), vec![], "", 1);
        let results = engine.suggest_sync(&ctx, Path::new("/"), "cd ").unwrap();
        assert!(
            !results.iter().any(|s| s.text == "../"),
            "../ should not appear at root: {results:?}"
        );
    }

    #[test]
    fn test_cd_parent_dir_absent_at_home() {
        let engine = make_engine();
        let home = std::env::var("HOME").unwrap();
        let ctx = make_ctx(Some("cd"), vec![], "", 1);
        let results = engine.suggest_sync(&ctx, Path::new(&home), "cd ").unwrap();
        assert!(
            !results.iter().any(|s| s.text == "../"),
            "../ should not appear at home dir: {results:?}"
        );
    }

    #[test]
    fn test_cd_chaining_offers_double_parent() {
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        let sub = tmp.path().join("aaa").join("bbb");
        std::fs::create_dir_all(&sub).unwrap();
        // Simulate: cd ../<TAB> from inside aaa/bbb
        let ctx = make_ctx(Some("cd"), vec![], "../", 1);
        let results = engine.suggest_sync(&ctx, &sub, "cd ../").unwrap();
        assert!(
            results.iter().any(|s| s.text == "../../"),
            "should offer ../../ when current_word is ../: {results:?}"
        );
    }

    #[test]
    fn test_cd_parent_dir_absent_with_query() {
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("mydir")).unwrap();
        // current_word = "my" — ../  doesn't match, should be filtered out
        let ctx = make_ctx(Some("cd"), vec![], "my", 1);
        let results = engine.suggest_sync(&ctx, tmp.path(), "cd my").unwrap();
        assert!(
            !results.iter().any(|s| s.text == "../"),
            "../ should be filtered out when current_word doesn't match: {results:?}"
        );
    }

    #[test]
    fn test_disabled_commands_provider() {
        let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap().store;
        let history = HistoryProvider::from_entries(vec![]);
        let commands = CommandsProvider::from_list(vec!["git".into(), "ls".into()]);
        let engine = SuggestionEngine::with_providers(spec_store, history, commands)
            .with_suggest_config(50, 10_000, false, 5, true, true, true);

        let ctx = make_ctx(None, vec![], "gi", 0);
        let results = engine.suggest_sync(&ctx, Path::new("/tmp"), "gi").unwrap();
        // Commands provider disabled — should not find "git" from commands
        assert!(
            !results
                .iter()
                .any(|s| s.source == crate::types::SuggestionSource::Commands),
            "should not have commands when provider disabled"
        );
    }

    #[test]
    fn test_history_matches_full_buffer_at_arg_position() {
        let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap().store;
        let history = HistoryProvider::from_entries(vec![
            "git push origin main".into(),
            "git checkout -b feature".into(),
        ]);
        let commands = CommandsProvider::from_list(vec!["git".into()]);
        let engine = SuggestionEngine::with_providers(spec_store, history, commands);

        let ctx = make_ctx(Some("git"), vec!["push"], "", 2);
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "git push ")
            .unwrap();
        let hist: Vec<_> = results
            .iter()
            .filter(|s| s.source == crate::types::SuggestionSource::History)
            .collect();
        assert!(
            hist.iter().any(|s| s.text == "git push origin main"),
            "expected full history entry in results: {hist:?}"
        );
    }

    #[tokio::test]
    async fn test_suggest_dynamic_with_script_generator() {
        let spec_json = r#"{
            "name": "test-dynamic",
            "args": [{
                "generators": [{
                    "script": ["printf", "alpha\nbeta\ngamma"],
                    "transforms": ["split_lines", "filter_empty"]
                }]
            }]
        }"#;
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("test-dynamic.json"), spec_json).unwrap();

        let engine = SuggestionEngine::new(dir.path()).unwrap();
        let ctx = make_ctx(Some("test-dynamic"), vec![], "", 1);
        let results = engine
            .suggest_dynamic(&ctx, Path::new("/tmp"), 5000)
            .await
            .unwrap();
        assert!(
            results.iter().any(|s| s.text == "alpha"),
            "expected 'alpha' in results: {results:?}"
        );
        assert!(
            results.iter().any(|s| s.text == "beta"),
            "expected 'beta' in results: {results:?}"
        );
        assert!(
            results.iter().any(|s| s.text == "gamma"),
            "expected 'gamma' in results: {results:?}"
        );
    }

    #[tokio::test]
    async fn test_suggest_dynamic_no_script_generators() {
        // A spec with only native generators should return empty from suggest_dynamic
        let spec_json = r#"{
            "name": "test-native-only",
            "args": [{"generators": [{"type": "git_branches"}]}]
        }"#;
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("test-native-only.json"), spec_json).unwrap();

        let engine = SuggestionEngine::new(dir.path()).unwrap();
        let ctx = make_ctx(Some("test-native-only"), vec![], "", 1);
        let results = engine
            .suggest_dynamic(&ctx, Path::new("/tmp"), 5000)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_suggest_dynamic_caches_results() {
        // Use date +%s%N to produce a non-deterministic value. If the cache
        // works, the second call returns the SAME stale result.
        let spec_json = r#"{
            "name": "test-cached",
            "args": [{
                "generators": [{
                    "script": ["date", "+%s%N"],
                    "transforms": ["split_lines", "filter_empty"],
                    "cache": {"ttl_seconds": 300}
                }]
            }]
        }"#;
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("test-cached.json"), spec_json).unwrap();

        let engine = SuggestionEngine::new(dir.path()).unwrap();
        let ctx = make_ctx(Some("test-cached"), vec![], "", 1);

        // First call populates cache
        let results = engine
            .suggest_dynamic(&ctx, Path::new("/tmp"), 5000)
            .await
            .unwrap();
        assert!(
            !results.is_empty(),
            "expected at least one result from date"
        );
        let first_value = results[0].text.clone();

        // Brief sleep so date would produce a different value if re-executed
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Second call should hit cache — returns the SAME value, proving cache hit
        let results2 = engine
            .suggest_dynamic(&ctx, Path::new("/tmp"), 5000)
            .await
            .unwrap();
        assert_eq!(
            results2[0].text, first_value,
            "second call should return cached (stale) value"
        );
    }

    #[tokio::test]
    async fn test_suggest_dynamic_command_position_returns_empty() {
        // word_index == 0 means command position — no dynamic suggestions
        let dir = tempfile::TempDir::new().unwrap();
        let engine = SuggestionEngine::new(dir.path()).unwrap();
        let ctx = make_ctx(None, vec![], "gi", 0);
        let results = engine
            .suggest_dynamic(&ctx, Path::new("/tmp"), 5000)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_history_capped_to_max_history_results() {
        let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap().store;
        let history = HistoryProvider::from_entries(vec![
            "git push origin main".into(),
            "git pull origin main".into(),
            "git fetch --all".into(),
            "git status".into(),
            "git log --oneline".into(),
        ]);
        let commands = CommandsProvider::from_list(vec!["git".into()]);
        let engine = SuggestionEngine::with_providers(spec_store, history, commands)
            .with_max_history_results(3);

        let ctx = make_ctx(None, vec![], "git", 0);
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "git")
            .unwrap();
        let hist_count = results
            .iter()
            .filter(|s| s.source == crate::types::SuggestionSource::History)
            .count();
        assert_eq!(
            hist_count, 3,
            "history should be capped at 3, got {hist_count}"
        );
    }

    #[test]
    fn test_history_disabled_when_max_zero() {
        let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap().store;
        let history = HistoryProvider::from_entries(vec![
            "git push origin main".into(),
            "cargo build".into(),
        ]);
        let commands = CommandsProvider::from_list(vec!["git".into(), "cargo".into()]);
        let engine = SuggestionEngine::with_providers(spec_store, history, commands)
            .with_max_history_results(0);

        let ctx = make_ctx(None, vec![], "git", 0);
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "git")
            .unwrap();
        let hist_count = results
            .iter()
            .filter(|s| s.source == crate::types::SuggestionSource::History)
            .count();
        assert_eq!(hist_count, 0, "history should be disabled when max is 0");
    }

    #[test]
    fn test_resolve_script_argv_static() {
        let gen = crate::specs::GeneratorSpec {
            generator_type: None,
            script: Some(vec!["echo".into(), "hello".into()]),
            script_template: None,
            transforms: vec![],
            cache: None,
            requires_js: false,
            js_source: None,
            template: None,
        };
        let ctx = make_ctx(Some("test"), vec![], "", 1);
        let argv = super::resolve_script_argv(&gen, &ctx);
        assert_eq!(argv, vec!["echo", "hello"]);
    }

    #[test]
    fn test_resolve_script_argv_template() {
        let gen = crate::specs::GeneratorSpec {
            generator_type: None,
            script: None,
            script_template: Some(vec!["cmd".into(), "{prev_token}".into()]),
            transforms: vec![],
            cache: None,
            requires_js: false,
            js_source: None,
            template: None,
        };
        let ctx = make_ctx(Some("test"), vec!["arg1"], "", 2);
        let argv = super::resolve_script_argv(&gen, &ctx);
        assert_eq!(argv, vec!["cmd", "arg1"]);
    }
}
