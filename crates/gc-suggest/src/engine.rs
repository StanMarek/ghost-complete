use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gc_buffer::CommandContext;
use tokio::sync::Semaphore;

use crate::alias::AliasStore;
use crate::cache::{CacheKey, GeneratorCache};
use crate::commands::CommandsProvider;
use crate::env::EnvProvider;
use crate::filesystem::FilesystemProvider;
use crate::frecency::FrecencyDb;
use crate::fuzzy;
use crate::git;
use crate::history::{HistoryProvider, DEFAULT_MAX_HISTORY_ENTRIES};
use crate::provider::Provider;
use crate::script::{run_script, substitute_template};
use crate::specs::{self, GeneratorSpec, SpecStore};
use crate::ssh::SshHostCache;
use crate::transform::execute_pipeline;
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

/// Maximum number of concurrent script generators.
const MAX_CONCURRENT_GENERATORS: usize = 3;

/// Cap on the candidate pool returned by async dynamic providers
/// (`run_generators`, `resolve_git`) **when the spawn-time query is
/// non-empty**. Passed as the `max_results` argument to `fuzzy::rank` at the
/// tail of each async body, so the survivors are the top-N *by score*
/// against the spawn-time query — not by generator order.
///
/// For the **empty-query case** (e.g. the user triggers completion on the
/// space after a command name, before typing any characters), the cap is
/// bypassed entirely: the raw merged pool is returned without calling
/// `fuzzy::rank`. Empty-query `fuzzy::rank` sorts by `(kind_priority,
/// text)` and truncates, which for single-kind pools (all `GitBranch`,
/// etc.) degenerates into an alphabetic position truncate — exactly the
/// failure mode we're trying to avoid. See `run_generators` for the full
/// rationale.
///
/// Rationale for the non-empty-query cap:
/// - The handler's `try_merge_dynamic` re-ranks the merged pool against the
///   CURRENT `current_word` under the handler lock. An unbounded pool would
///   make lock-hold time scale with raw provider output (e.g. 10k git
///   branches on a giant repo), starving the stdin/PTY/SIGWINCH tasks.
/// - A pure size truncate ("first N items") was tried and rejected: git
///   providers emit refname-alphabetic order, so "first 1000" can miss every
///   match on a large monorepo that happens to sort late alphabetically.
///   Ranking against the spawn-time query filters non-matches first, so the
///   cap trims the long tail of matches by score rather than by position.
/// - 1000 is ~20x the visible result count (`DEFAULT_MAX_RESULTS = 50`) — a
///   generous headroom that leaves the stale-query bug (the reason the
///   original `max_results = 50` rank was removed) as a narrow theoretical
///   case rather than a common failure mode. Nucleo's benchmark target of
///   <1ms on 10k candidates means a locked re-rank of ≤1000 stays well
///   under the keystroke-latency budget.
const MAX_DYNAMIC_CANDIDATES: usize = 1000;

/// Result from `suggest_sync` — includes ranked suggestions and any
/// generators that the caller should dispatch asynchronously.
#[derive(Debug)]
pub struct SyncResult {
    pub suggestions: Vec<Suggestion>,
    /// Script generators from the spec resolution, if any. The caller passes
    /// these to `run_generators` to avoid re-resolving the spec tree.
    ///
    /// `Arc<GeneratorSpec>` not `GeneratorSpec`: this vec is cloned on the
    /// hot path (handler snapshots it before spawning the async task) and
    /// each element carries `Vec<Transform>`/`Vec<String>` argv that we do
    /// NOT want to deep-copy on every keystroke trigger.
    pub script_generators: Vec<Arc<specs::GeneratorSpec>>,
    /// Native git generators resolved from the spec. The caller dispatches
    /// these asynchronously via `resolve_git` to avoid blocking the runtime.
    pub git_generators: Vec<git::GitQueryKind>,
}

impl SyncResult {
    /// Iterate over the ranked suggestions (convenience for callers and tests).
    pub fn iter(&self) -> std::slice::Iter<'_, Suggestion> {
        self.suggestions.iter()
    }

    /// True when there are ranked suggestions to display.
    /// Note: script_generators may still be present even when this returns false.
    pub fn has_suggestions(&self) -> bool {
        !self.suggestions.is_empty()
    }
}

pub struct SuggestionEngine {
    spec_store: SpecStore,
    filesystem_provider: FilesystemProvider,
    history_provider: HistoryProvider,
    commands_provider: CommandsProvider,
    env_provider: EnvProvider,
    ssh_host_cache: Option<SshHostCache>,
    alias_map: AliasStore,
    generator_cache: Arc<GeneratorCache>,
    frecency_db: FrecencyDb,
    max_results: usize,
    max_history_results: usize,
    providers_commands: bool,
    providers_filesystem: bool,
    providers_specs: bool,
    providers_git: bool,
}

impl SuggestionEngine {
    pub fn new(spec_dirs: &[PathBuf]) -> Result<Self> {
        let result = SpecStore::load_from_dirs(spec_dirs)?;
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
            env_provider: EnvProvider::new(),
            ssh_host_cache: SshHostCache::default_path(),
            alias_map: AliasStore::load_async(),
            generator_cache: Arc::new(GeneratorCache::new()),
            frecency_db: FrecencyDb::load(),
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
            self.history_provider = HistoryProvider::load(DEFAULT_MAX_HISTORY_ENTRIES);
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
            env_provider: EnvProvider::new(),
            ssh_host_cache: SshHostCache::default_path(),
            alias_map: AliasStore::empty(),
            generator_cache: Arc::new(GeneratorCache::new()),
            frecency_db: FrecencyDb::empty(),
            max_results: fuzzy::DEFAULT_MAX_RESULTS,
            max_history_results: 5,
            providers_commands: true,
            providers_filesystem: true,
            providers_specs: true,
            providers_git: true,
        }
    }

    /// Record an accepted completion for frecency scoring.
    /// `command` scopes the key so `--help` under `git` doesn't boost `docker`.
    /// `kind` scopes it further so a branch `main` doesn't boost a file `main`.
    pub fn record_frecency(&self, command: Option<&str>, kind: SuggestionKind, text: &str) {
        let key = crate::frecency::frecency_key(command, kind, text);
        self.frecency_db.record(&key);
    }

    /// Flush unsaved frecency records to disk. Call on shutdown.
    pub fn flush_frecency(&self) {
        self.frecency_db.flush();
    }

    /// Test helper — set the history results cap without reloading from disk.
    #[cfg(test)]
    pub fn with_max_history_results(mut self, n: usize) -> Self {
        self.max_history_results = n;
        self
    }

    /// Test helper — inject a custom SSH config path for deterministic tests.
    #[cfg(test)]
    pub fn with_ssh_config(mut self, path: std::path::PathBuf) -> Self {
        self.ssh_host_cache = Some(SshHostCache::new(path));
        self
    }

    /// Run pre-resolved script generators. Called by the handler with generators
    /// obtained from `SyncResult::script_generators`, avoiding redundant spec
    /// resolution.
    pub async fn run_generators(
        &self,
        generators: &[Arc<specs::GeneratorSpec>],
        ctx: &CommandContext,
        cwd: &Path,
        timeout_ms: u64,
    ) -> Result<Vec<Suggestion>> {
        if generators.is_empty() {
            return Ok(Vec::new());
        }

        let command = match &ctx.command {
            Some(c) => c,
            None => return Ok(Vec::new()),
        };

        let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_GENERATORS));
        let mut handles = Vec::new();

        for gen in generators {
            // Borrow the inner GeneratorSpec once; we pass references into
            // cache-keying and transform-cloning below just like before the
            // Arc wrapper was added.
            let gen: &specs::GeneratorSpec = gen.as_ref();
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

        // Empty spawn-time query: the common "trigger on space after a
        // command, then type" case. There's no relevance signal to filter
        // or rank against, so we MUST NOT call `fuzzy::rank` — its
        // empty-query path sorts by `(kind_priority, text)` and truncates,
        // which for single-kind providers (e.g. all `GitBranch`) collapses
        // to an alphabetic position truncate. That reintroduces the exact
        // false-negative we just fixed for non-empty queries: in a
        // 5000-branch monorepo, `zzz-hotfix-critical` drops alphabetically
        // past position 1000 before the user has typed a single character.
        //
        // Return the raw merged pool instead. The handler's
        // `try_merge_dynamic` re-ranks against the user's EVENTUAL typed
        // query, bounded by its own `max_visible * 5` cap. Per the nucleo
        // performance target in `CLAUDE.md` (<1ms on 10k candidates), the
        // locked re-rank stays within the keystroke budget for realistic
        // provider outputs. For pathological providers (>10k items), the
        // fully-correct fix is moving the handler re-rank outside the
        // mutex (Option B) — cross-crate refactor deferred.
        if ctx.current_word.is_empty() {
            return Ok(all_results);
        }

        // Non-empty query: spawn-time `fuzzy::rank` at a generous cap
        // (`MAX_DYNAMIC_CANDIDATES`).
        //
        // Why rank here rather than a pure size truncate: provider output
        // order is NOT relevance order for alphabetic providers — `git
        // branch --format=%(refname:short)` and `git tag --list` emit in
        // refname-alphabetic order. A position truncate on a 5000-branch
        // monorepo would silently drop every match past refname position
        // 1000, regardless of how well the user's query matches it.
        //
        // Why this is NOT the previously-fixed stale-query bug resurfacing:
        // the original bug was `max_results = DEFAULT_MAX_RESULTS (50)` — so
        // tight that a user typing more characters routinely found matches
        // already evicted at spawn time. Here the cap is 1000, ~20x the
        // visible result count, so the cap only trims pools that are deeply
        // long-tail under the spawn-time query, and the handler's
        // `try_merge_dynamic` re-ranks the survivors against the CURRENT
        // query at merge time.
        //
        // Known limitation: for pools with >1000 matching candidates, a
        // narrow scoring edge case may drop a candidate that would score
        // higher under an extended query (e.g. mid-word `h` scoring low for
        // `"h"` but a contiguous mid-word `ho` scoring high for `"ho"`).
        // Full correctness here also requires Option B.
        Ok(fuzzy::rank(
            &ctx.current_word,
            all_results,
            MAX_DYNAMIC_CANDIDATES,
        ))
    }

    /// Resolve native git generators asynchronously using `tokio::process::Command`.
    /// Called by the handler alongside `run_generators`.
    pub async fn resolve_git(
        &self,
        kinds: &[git::GitQueryKind],
        cwd: &Path,
        query: &str,
    ) -> Result<Vec<Suggestion>> {
        let mut all = Vec::new();
        for &kind in kinds {
            match git::git_suggestions(cwd, kind).await {
                Ok(suggestions) => all.extend(suggestions),
                Err(e) => tracing::debug!("git provider error ({kind:?}): {e}"),
            }
        }
        // Empty spawn-time query: return the raw pool (no `fuzzy::rank`
        // call). `fuzzy::rank`'s empty-query path sorts by kind+text and
        // truncates, which for all-GitBranch pools collapses to an
        // alphabetic position truncate — reintroducing the `zzz-hotfix`
        // false-negative on large monorepos. The handler re-ranks against
        // the user's eventual typed query. See `run_generators` for the
        // full rationale.
        if query.is_empty() {
            return Ok(all);
        }
        // Non-empty query: rank at the generous cap. Git providers emit
        // refname-alphabetic order, so a pure size truncate would
        // guarantee false negatives past position ~1000 in large
        // monorepos. See `run_generators` for the full rationale and the
        // known edge-case limitation.
        Ok(fuzzy::rank(query, all, MAX_DYNAMIC_CANDIDATES))
    }

    /// Convenience method that resolves the spec and runs script generators.
    /// Prefer `run_generators` in the handler to avoid redundant spec resolution.
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
        let generators: Vec<_> = resolution
            .script_generators
            .into_iter()
            .filter(|g| !g.requires_js)
            .collect();
        self.run_generators(&generators, ctx, cwd, timeout_ms).await
    }

    /// Dispatcher for the synchronous suggestion pipeline. Each branch is
    /// handled by a focused helper; this method only picks the right one
    /// based on the cursor context.
    pub fn suggest_sync(
        &self,
        ctx: &CommandContext,
        cwd: &Path,
        buffer: &str,
    ) -> Result<SyncResult> {
        // Command position: commands (history handled by rank_with_history).
        if ctx.word_index == 0 {
            return Ok(self.suggest_command_position(ctx, cwd, buffer));
        }

        // Redirect: always filesystem.
        if ctx.in_redirect {
            return Ok(self.suggest_redirect(ctx, cwd, buffer));
        }

        // Contextual injectors populate the shared candidate set before
        // spec/filesystem resolution runs.
        let mut candidates = Vec::new();
        self.extend_with_env_vars(ctx, cwd, &mut candidates);
        self.extend_with_ssh_hosts(ctx, &mut candidates);

        // Spec-driven completion takes priority over the path heuristic.
        // On Err, try_suggest_from_spec hands the partially-populated
        // candidates vec back so the caller can fall through.
        let candidates = match self.try_suggest_from_spec(ctx, cwd, buffer, candidates) {
            Ok(result) => return Ok(result),
            Err(candidates) => candidates,
        };

        // Path-like current_word vs. plain fallback: same logic, different
        // tracing label for debugging.
        let label = if looks_like_path(&ctx.current_word) {
            "path"
        } else {
            "fallback"
        };
        Ok(self.suggest_filesystem_fallback(ctx, cwd, buffer, candidates, label))
    }

    /// Complete the command name (`ctx.word_index == 0`). Pulls candidates
    /// from the `$PATH` commands provider; history is injected by
    /// `rank_with_history`.
    fn suggest_command_position(
        &self,
        ctx: &CommandContext,
        cwd: &Path,
        buffer: &str,
    ) -> SyncResult {
        let mut candidates = Vec::new();
        if self.providers_commands {
            match self.commands_provider.provide(ctx, cwd) {
                Ok(cmds) => candidates.extend(cmds),
                Err(e) => tracing::warn!("commands provider error: {e}"),
            }
        }
        SyncResult {
            suggestions: self.rank_with_history(ctx, cwd, buffer, candidates),
            script_generators: Vec::new(),
            git_generators: Vec::new(),
        }
    }

    /// Complete after a redirect operator (e.g. `echo foo > <TAB>`). The
    /// shell will write to a file, so only filesystem candidates are
    /// relevant — not commands, not specs.
    fn suggest_redirect(&self, ctx: &CommandContext, cwd: &Path, buffer: &str) -> SyncResult {
        let mut candidates = Vec::new();
        if self.providers_filesystem {
            match self.filesystem_provider.provide(ctx, cwd) {
                Ok(fs) => candidates.extend(fs),
                Err(e) => tracing::warn!("filesystem provider error (redirect): {e}"),
            }
        }
        SyncResult {
            suggestions: self.rank_with_history(ctx, cwd, buffer, candidates),
            script_generators: Vec::new(),
            git_generators: Vec::new(),
        }
    }

    /// Inject environment variable candidates when `current_word` starts
    /// with `$`. Augments the candidate set without short-circuiting spec
    /// or filesystem resolution.
    fn extend_with_env_vars(
        &self,
        ctx: &CommandContext,
        cwd: &Path,
        candidates: &mut Vec<Suggestion>,
    ) {
        if !ctx.current_word.starts_with('$') {
            return;
        }
        match self.env_provider.provide(ctx, cwd) {
            Ok(env_vars) => candidates.extend(env_vars),
            Err(e) => tracing::warn!("env provider error: {e}"),
        }
    }

    /// Inject SSH host candidates when completing an argument to `ssh`
    /// (respecting alias resolution). Skips the command position and flag
    /// words so hosts don't appear for `ssh -p<TAB>` or unrelated commands.
    fn extend_with_ssh_hosts(&self, ctx: &CommandContext, candidates: &mut Vec<Suggestion>) {
        let Some(cache) = self.ssh_host_cache.as_ref() else {
            return;
        };
        let Some(cmd) = ctx.command.as_ref() else {
            return;
        };
        let resolved_owned = self.alias_map.get(cmd.as_str());
        let resolved_cmd = resolved_owned.as_deref().unwrap_or(cmd.as_str());
        if resolved_cmd != "ssh" || ctx.word_index == 0 || ctx.is_flag {
            return;
        }
        candidates.extend(
            cache
                .hosts_matching(&ctx.current_word)
                .into_iter()
                .map(|host| Suggestion {
                    text: host,
                    description: Some("SSH host".to_string()),
                    kind: SuggestionKind::Command,
                    source: SuggestionSource::SshConfig,
                    ..Default::default()
                }),
        );
    }

    /// Attempt to resolve candidates from a loaded spec for this command.
    ///
    /// Returns `Ok(SyncResult)` if a spec was found and handled. Returns
    /// `Err(candidates)` — handing the partially-populated candidate vec
    /// back — so the caller can fall through to filesystem completion.
    fn try_suggest_from_spec(
        &self,
        ctx: &CommandContext,
        cwd: &Path,
        buffer: &str,
        mut candidates: Vec<Suggestion>,
    ) -> std::result::Result<SyncResult, Vec<Suggestion>> {
        if !self.providers_specs {
            return Err(candidates);
        }
        let Some(command) = ctx.command.as_ref() else {
            return Err(candidates);
        };
        // Resolve alias: if "g" is aliased to "git", look up "git" spec.
        let resolved_owned = self.alias_map.get(command.as_str());
        let resolved = resolved_owned.as_deref().unwrap_or(command.as_str());
        let Some(spec) = self.spec_store.get(resolved) else {
            return Err(candidates);
        };

        let specs::SpecResolution {
            subcommands,
            options,
            native_generators,
            script_generators,
            wants_filepaths,
            wants_folders_only,
            preceding_flag_has_args,
            past_double_dash,
        } = specs::resolve_spec(spec, ctx);

        // Suppress subcommands/options when:
        // 1. The preceding flag takes an argument (e.g. `curl -o <TAB>`)
        // 2. We're past `--` (end-of-flags separator) — only positional args
        let suppress_commands = preceding_flag_has_args || past_double_dash;

        if !suppress_commands {
            candidates.extend(subcommands);
            candidates.extend(options);
        }

        let git_generators = self.git_generators_from(&native_generators);

        // Add filesystem: folders-only or all filepaths.
        if wants_folders_only && self.providers_filesystem {
            self.extend_with_folders(ctx, cwd, &mut candidates);
        } else if wants_filepaths && self.providers_filesystem {
            match self.filesystem_provider.provide(ctx, cwd) {
                Ok(fs) => candidates.extend(fs),
                Err(e) => tracing::warn!("filesystem provider error: {e}"),
            }
        }

        // Script generators are dispatched asynchronously by the caller.
        let script_generators: Vec<_> = script_generators
            .into_iter()
            .filter(|g| !g.requires_js)
            .collect();

        Ok(SyncResult {
            suggestions: self.rank_with_history(ctx, cwd, buffer, candidates),
            script_generators,
            git_generators,
        })
    }

    /// Collect native git generators for async resolution by the caller.
    /// Previously these ran synchronously via `std::process::Command`,
    /// blocking the tokio runtime thread for 200-500ms on large repos.
    fn git_generators_from(&self, native_generators: &[String]) -> Vec<git::GitQueryKind> {
        if !self.providers_git {
            return Vec::new();
        }
        native_generators
            .iter()
            .filter_map(|g| git::generator_to_query_kind(g))
            .collect()
    }

    /// Populate `candidates` with directory-only filesystem results plus an
    /// optional "../" parent-directory entry. Used by spec arguments whose
    /// `template` is `"folders"` (e.g. `cd`).
    fn extend_with_folders(
        &self,
        ctx: &CommandContext,
        cwd: &Path,
        candidates: &mut Vec<Suggestion>,
    ) {
        // Offer "../" to navigate up, unless already at / or $HOME.
        let parent_text = if ctx.current_word.is_empty() {
            Some("../".to_string())
        } else if ctx.current_word.ends_with("../") {
            Some(format!("{}../", ctx.current_word))
        } else {
            None
        };
        if let Some(text) = parent_text {
            let effective = cwd.join(&ctx.current_word);
            let at_boundary = effective.canonicalize().ok().is_none_or(|resolved| {
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
            Err(e) => tracing::warn!("filesystem provider error (folders): {e}"),
        }
    }

    /// Extend `candidates` with filesystem results and rank. Used when no
    /// spec matches — either because `current_word` looks like a path or
    /// as a final fallback. `label` appears in the tracing log only.
    fn suggest_filesystem_fallback(
        &self,
        ctx: &CommandContext,
        cwd: &Path,
        buffer: &str,
        mut candidates: Vec<Suggestion>,
        label: &'static str,
    ) -> SyncResult {
        if self.providers_filesystem {
            match self.filesystem_provider.provide(ctx, cwd) {
                Ok(fs) => candidates.extend(fs),
                Err(e) => tracing::warn!("filesystem provider error ({label}): {e}"),
            }
        }
        SyncResult {
            suggestions: self.rank_with_history(ctx, cwd, buffer, candidates),
            script_generators: Vec::new(),
            git_generators: Vec::new(),
        }
    }

    /// Rank main candidates with current_word, then separately rank history
    /// candidates with the full buffer, and append history results at the end.
    /// All suggestions receive a frecency bonus so frequently/recently accepted
    /// completions sort higher.
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
                        results.extend(fuzzy::rank(buffer, hist, remaining));
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("history provider error: {e}"),
                }
            }
        }

        // Apply frecency boost to ALL suggestions, then re-sort.
        // Re-sort after frecency boost changes scores. Maintains history-comes-last partition.
        self.frecency_db
            .boost_scores(&mut results, ctx.command.as_deref());
        results.sort_by(|a, b| {
            let a_hist = a.source == SuggestionSource::History;
            let b_hist = b.source == SuggestionSource::History;
            a_hist
                .cmp(&b_hist)
                .then_with(|| b.score.cmp(&a.score))
                .then_with(|| a.kind.sort_priority().cmp(&b.kind.sort_priority()))
                .then_with(|| a.text.cmp(&b.text))
        });

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
        assert!(results.suggestions.is_empty());
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
    fn test_option_arg_script_generator_suppresses_subcommands() {
        // When a flag's arg has script generators, the in_option_arg guard
        // must suppress subcommands/options. The guard must cover script
        // generators as well as templates and native generators.
        let spec_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            spec_dir.path().join("test-script-arg.json"),
            r#"{
                "name": "test-script-arg",
                "subcommands": [
                    {
                        "name": "deploy",
                        "options": [
                            {
                                "name": ["--env"],
                                "description": "Target environment",
                                "args": {
                                    "name": "env",
                                    "generators": [{
                                        "script": ["printf", "staging\nproduction"]
                                    }]
                                }
                            }
                        ],
                        "subcommands": [
                            {"name": "canary", "description": "Canary deploy"}
                        ]
                    }
                ]
            }"#,
        )
        .unwrap();
        let spec_store = SpecStore::load_from_dir(spec_dir.path()).unwrap().store;
        let history = HistoryProvider::from_entries(vec![]);
        let commands = CommandsProvider::from_list(vec![]);
        let engine = SuggestionEngine::with_providers(spec_store, history, commands);

        // Simulate: test-script-arg deploy --env <TAB>
        let ctx = CommandContext {
            command: Some("test-script-arg".into()),
            args: vec!["deploy".into(), "--env".into()],
            current_word: String::new(),
            word_index: 3,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: Some("--env".into()),
            in_pipe: false,
            in_redirect: false,
            quote_state: QuoteState::None,
            is_first_segment: true,
        };
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "test-script-arg deploy --env ")
            .unwrap();

        // Subcommands and options should be suppressed
        assert!(
            !results.iter().any(|s| s.text == "canary"),
            "subcommand 'canary' should be suppressed when flag has script generator arg: {results:?}"
        );
        assert!(
            !results.iter().any(|s| s.text == "--env"),
            "option '--env' should be suppressed when flag has script generator arg: {results:?}"
        );
        // Script generators should be present for async dispatch
        assert!(
            !results.script_generators.is_empty(),
            "script generators should be returned for async dispatch"
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
        assert!(
            !results.suggestions.is_empty(),
            "cd should return suggestions"
        );
        assert_eq!(
            results.suggestions[0].text, "../",
            "first cd suggestion should be ../, got: {:?}",
            results.suggestions[0].text
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
            .with_suggest_config(50, false, 5, true, true, true);

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

        let dirs = vec![dir.path().to_path_buf()];
        let engine = SuggestionEngine::new(&dirs).unwrap();
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

        let dirs = vec![dir.path().to_path_buf()];
        let engine = SuggestionEngine::new(&dirs).unwrap();
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

        let dirs = vec![dir.path().to_path_buf()];
        let engine = SuggestionEngine::new(&dirs).unwrap();
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
        let dirs = vec![dir.path().to_path_buf()];
        let engine = SuggestionEngine::new(&dirs).unwrap();
        let ctx = make_ctx(None, vec![], "gi", 0);
        let results = engine
            .suggest_dynamic(&ctx, Path::new("/tmp"), 5000)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_suggest_sync_returns_git_generators_not_inline() {
        // Git generators must be returned for async dispatch, not resolved
        // inline (which would block the tokio runtime).
        let spec_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            spec_dir.path().join("test-git-gen.json"),
            r#"{
                "name": "test-git-gen",
                "args": [{"generators": [{"type": "git_branches"}]}]
            }"#,
        )
        .unwrap();
        let spec_store = SpecStore::load_from_dir(spec_dir.path()).unwrap().store;
        let history = HistoryProvider::from_entries(vec![]);
        let commands = CommandsProvider::from_list(vec![]);
        let engine = SuggestionEngine::with_providers(spec_store, history, commands);

        let ctx = make_ctx(Some("test-git-gen"), vec![], "", 1);
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "test-git-gen ")
            .unwrap();
        // The git generators should be deferred, not resolved inline
        assert!(
            !results.git_generators.is_empty(),
            "git generators should be returned for async dispatch, got: {:?}",
            results.git_generators
        );
        assert_eq!(
            results.git_generators[0],
            crate::git::GitQueryKind::Branches,
        );
    }

    #[tokio::test]
    async fn test_resolve_git_returns_branches() {
        // resolve_git must work asynchronously.
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        if !workspace_root.join(".git").exists() {
            return; // skip if not in a git repo
        }
        let engine = make_engine();
        let results = engine
            .resolve_git(&[crate::git::GitQueryKind::Branches], &workspace_root, "")
            .await
            .unwrap();
        assert!(
            !results.is_empty(),
            "expected at least one branch from resolve_git"
        );
        assert!(
            results.iter().all(|s| s.kind == SuggestionKind::GitBranch),
            "all results should be GitBranch kind"
        );
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
        let results = engine.suggest_sync(&ctx, Path::new("/tmp"), "git").unwrap();
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
        let results = engine.suggest_sync(&ctx, Path::new("/tmp"), "git").unwrap();
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

    #[test]
    fn test_ssh_host_completion_injected() {
        let dir = tempfile::TempDir::new().unwrap();
        let ssh_config = dir.path().join("config");
        std::fs::write(&ssh_config, "Host prod\n    HostName prod.example.com\n\nHost staging\n    HostName staging.example.com\n").unwrap();

        let engine = make_engine().with_ssh_config(ssh_config);
        let ctx = make_ctx(Some("ssh"), vec![], "", 1);
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "ssh ")
            .unwrap();
        let ssh_results: Vec<_> = results
            .iter()
            .filter(|s| s.source == crate::types::SuggestionSource::SshConfig)
            .collect();
        assert!(
            ssh_results.iter().any(|s| s.text == "prod"),
            "expected 'prod' in SSH results: {ssh_results:?}"
        );
        assert!(
            ssh_results.iter().any(|s| s.text == "staging"),
            "expected 'staging' in SSH results: {ssh_results:?}"
        );
    }

    #[test]
    fn test_ssh_host_completion_not_for_flags() {
        let dir = tempfile::TempDir::new().unwrap();
        let ssh_config = dir.path().join("config");
        std::fs::write(&ssh_config, "Host myhost\n").unwrap();

        let engine = make_engine().with_ssh_config(ssh_config);
        // Typing a flag: ssh -p  — should not inject hosts
        let ctx = make_ctx(Some("ssh"), vec![], "-p", 1);
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "ssh -p")
            .unwrap();
        let ssh_results: Vec<_> = results
            .iter()
            .filter(|s| s.source == crate::types::SuggestionSource::SshConfig)
            .collect();
        assert!(
            ssh_results.is_empty(),
            "SSH hosts should not appear when typing a flag: {ssh_results:?}"
        );
    }

    #[test]
    fn test_ssh_host_completion_not_for_other_commands() {
        let dir = tempfile::TempDir::new().unwrap();
        let ssh_config = dir.path().join("config");
        std::fs::write(&ssh_config, "Host myhost\n").unwrap();

        let engine = make_engine().with_ssh_config(ssh_config);
        let ctx = make_ctx(Some("git"), vec![], "", 1);
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "git ")
            .unwrap();
        let ssh_results: Vec<_> = results
            .iter()
            .filter(|s| s.source == crate::types::SuggestionSource::SshConfig)
            .collect();
        assert!(
            ssh_results.is_empty(),
            "SSH hosts should not appear for non-ssh commands: {ssh_results:?}"
        );
    }

    #[test]
    fn test_ssh_host_fuzzy_filtered() {
        let dir = tempfile::TempDir::new().unwrap();
        let ssh_config = dir.path().join("config");
        std::fs::write(&ssh_config, "Host prod staging dev\n").unwrap();

        let engine = make_engine().with_ssh_config(ssh_config);
        let ctx = make_ctx(Some("ssh"), vec![], "pro", 1);
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "ssh pro")
            .unwrap();
        assert!(
            results.iter().any(|s| s.text == "prod"),
            "expected 'prod' to match fuzzy query 'pro': {results:?}"
        );
        // "staging" and "dev" should be filtered out by fuzzy ranking
        assert!(
            !results.iter().any(|s| s.text == "staging"),
            "'staging' should not match 'pro': {results:?}"
        );
    }

    // ---------------------------------------------------------------
    // rank_with_history re-sort tests (Issue #3 from PR review)
    // ---------------------------------------------------------------

    #[test]
    fn test_frecency_boost_reorders_non_history_suggestions() {
        // Record high frecency for "checkout" under git, nothing for "cherry-pick".
        // Both match query "ch" — checkout should sort above cherry-pick after boost.
        let engine = make_engine();

        // Boost "checkout" frecency under git
        for _ in 0..10 {
            engine.record_frecency(Some("git"), SuggestionKind::Subcommand, "checkout");
        }

        let ctx = make_ctx(Some("git"), vec![], "ch", 1);
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "git ch")
            .unwrap();

        let non_hist: Vec<_> = results
            .iter()
            .filter(|s| s.source != SuggestionSource::History)
            .collect();

        assert!(
            non_hist.len() >= 2,
            "need at least 2 results for ordering test"
        );
        let checkout_pos = non_hist.iter().position(|s| s.text == "checkout");
        let cherry_pick_pos = non_hist.iter().position(|s| s.text == "cherry-pick");

        if let (Some(co), Some(cp)) = (checkout_pos, cherry_pick_pos) {
            assert!(
                co < cp,
                "frecency-boosted 'checkout' should sort above 'cherry-pick', positions: checkout={co}, cherry-pick={cp}"
            );
        }
    }

    #[test]
    fn test_history_stays_last_despite_frecency() {
        // Even with massive frecency on a history entry, it should sort after
        // non-history entries.
        let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap().store;
        let history = HistoryProvider::from_entries(vec!["git push origin main".into()]);
        let commands = CommandsProvider::from_list(vec!["git".into()]);
        let engine = SuggestionEngine::with_providers(spec_store, history, commands);

        // Give "git push origin main" massive frecency (no command scope since it's history)
        for _ in 0..50 {
            engine.record_frecency(None, SuggestionKind::History, "git push origin main");
        }

        let ctx = make_ctx(None, vec![], "git", 0);
        let results = engine.suggest_sync(&ctx, Path::new("/tmp"), "git").unwrap();

        let history_indices: Vec<_> = results
            .suggestions
            .iter()
            .enumerate()
            .filter(|(_, s)| s.source == SuggestionSource::History)
            .map(|(i, _)| i)
            .collect();
        let non_history_indices: Vec<_> = results
            .suggestions
            .iter()
            .enumerate()
            .filter(|(_, s)| s.source != SuggestionSource::History)
            .map(|(i, _)| i)
            .collect();

        if !history_indices.is_empty() && !non_history_indices.is_empty() {
            let max_non_hist = *non_history_indices.last().unwrap();
            let min_hist = *history_indices.first().unwrap();
            assert!(
                min_hist > max_non_hist,
                "all history entries should come after non-history entries, \
                 non-hist max idx={max_non_hist}, hist min idx={min_hist}"
            );
        }
    }

    #[test]
    fn test_sort_priority_tiebreaker_with_equal_boosted_scores() {
        // When two suggestions have the same score after frecency boost,
        // sort_priority should break the tie (GitBranch < Subcommand < Flag).
        let engine = make_engine();
        let ctx = make_ctx(Some("git"), vec![], "ch", 1);
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "git ch")
            .unwrap();

        let non_hist: Vec<_> = results
            .iter()
            .filter(|s| s.source != SuggestionSource::History)
            .collect();

        // For any adjacent pair with equal scores, verify sort_priority ordering
        for pair in non_hist.windows(2) {
            if pair[0].score == pair[1].score {
                assert!(
                    pair[0].kind.sort_priority() <= pair[1].kind.sort_priority(),
                    "equal-score items should be ordered by sort_priority: {:?} (pri={}) before {:?} (pri={})",
                    pair[0].text, pair[0].kind.sort_priority(),
                    pair[1].text, pair[1].kind.sort_priority()
                );
            }
        }
    }

    #[test]
    fn test_context_scoping_prevents_cross_command_frecency_bleed() {
        // Record frecency for "--verbose" under "cargo", then query "docker --"
        // The frecency for cargo's --verbose should NOT affect docker's results.
        let engine = make_engine();

        for _ in 0..20 {
            engine.record_frecency(Some("cargo"), SuggestionKind::Flag, "--verbose");
        }

        let ctx = make_ctx(Some("docker"), vec![], "--", 1);
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "docker --")
            .unwrap();

        // If --verbose appears in docker results, its score should NOT be boosted
        if let Some(verbose) = results.iter().find(|s| s.text == "--verbose") {
            // Without frecency boost, the score should be from fuzzy matching only
            // A boosted score would be >= 2000 (20 records * 100 multiplier)
            assert!(
                verbose.score < 2000,
                "cargo's --verbose frecency should not leak to docker, score={}",
                verbose.score
            );
        }
    }
}
