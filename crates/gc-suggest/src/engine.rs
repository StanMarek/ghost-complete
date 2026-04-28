use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gc_buffer::CommandContext;
use tokio::sync::Semaphore;

use crate::alias::AliasStore;
use crate::alias_expand::expand_alias_for_spec;
use crate::cache::{CacheKey, GeneratorCache};
use crate::commands::CommandsProvider;
use crate::env::EnvProvider;
use crate::filesystem::FilesystemProvider;
use crate::frecency::FrecencyDb;
use crate::fuzzy;
use crate::git;
use crate::history::{HistoryProvider, DEFAULT_MAX_HISTORY_ENTRIES};
use crate::priority;
use crate::provider::Provider;
use crate::providers::{self, ProviderCtx, ProviderKind};
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
    /// Native providers resolved from the spec (e.g. `arduino_cli_boards`).
    /// The caller dispatches these asynchronously via `resolve_providers`.
    /// Carries pre-resolved `ProviderKind`s so the engine can dispatch
    /// without re-parsing the `"type"` string on the keystroke hot path.
    pub provider_generators: Vec<ProviderKind>,
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

    /// True iff any pending async generator's kind base priority outranks
    /// the highest-priority sync suggestion currently in `self.suggestions`.
    ///
    /// This is a conservative heuristic: git generators always conceptually
    /// produce branches/tags (highest base priority 80), script and provider
    /// generators produce ProviderValue (base 70). If the best sync item
    /// already has priority ≥ the expected async priority, there is no point
    /// waiting — the async results would not change the top of the list.
    pub fn has_pending_high_priority(&self) -> bool {
        let top_sync = self
            .suggestions
            .iter()
            .map(crate::priority::effective)
            .max()
            .unwrap_or_else(|| crate::priority::Priority::new(0));

        // Git generators produce GitBranch/GitTag — base priority 80.
        let git_base = crate::types::SuggestionKind::GitBranch.base_priority();
        // Script and provider generators produce ProviderValue — base priority 70.
        let provider_base = crate::types::SuggestionKind::ProviderValue.base_priority();

        (!self.git_generators.is_empty() && git_base > top_sync)
            || (!self.script_generators.is_empty() && provider_base > top_sync)
            || (!self.provider_generators.is_empty() && provider_base > top_sync)
    }
}

#[cfg(test)]
mod sync_result_tests {
    use super::*;
    use crate::types::{Suggestion, SuggestionKind};

    #[test]
    fn has_pending_high_priority_false_when_no_generators() {
        let result = SyncResult {
            suggestions: vec![],
            script_generators: vec![],
            git_generators: vec![],
            provider_generators: vec![],
        };
        assert!(!result.has_pending_high_priority());
    }

    #[test]
    fn has_pending_high_priority_true_when_git_pending_and_no_sync() {
        let result = SyncResult {
            suggestions: vec![],
            script_generators: vec![],
            git_generators: vec![crate::git::GitQueryKind::Branches],
            provider_generators: vec![],
        };
        // No sync suggestions → top_sync = 0 < 80 (GitBranch base)
        assert!(result.has_pending_high_priority());
    }

    #[test]
    fn has_pending_high_priority_false_when_sync_already_outranks_git() {
        let result = SyncResult {
            suggestions: vec![Suggestion {
                kind: SuggestionKind::GitBranch,
                priority: None,
                ..Default::default()
            }],
            script_generators: vec![],
            git_generators: vec![crate::git::GitQueryKind::Branches],
            provider_generators: vec![],
        };
        // top_sync = 80, git_base = 80 → NOT strictly greater → false
        assert!(!result.has_pending_high_priority());
    }

    #[test]
    fn has_pending_high_priority_true_when_git_pending_and_flags_only_in_sync() {
        let result = SyncResult {
            suggestions: vec![Suggestion {
                kind: SuggestionKind::Flag,
                priority: None,
                ..Default::default()
            }],
            script_generators: vec![],
            git_generators: vec![crate::git::GitQueryKind::Branches],
            provider_generators: vec![],
        };
        // top_sync = 30 (Flag), git_base = 80 → 80 > 30 → true
        assert!(result.has_pending_high_priority());
    }

    #[test]
    fn has_pending_high_priority_true_when_provider_pending_and_flags_only_in_sync() {
        let result = SyncResult {
            suggestions: vec![Suggestion {
                kind: SuggestionKind::Flag,
                priority: None,
                ..Default::default()
            }],
            script_generators: vec![],
            git_generators: vec![],
            provider_generators: vec![ProviderKind::DefaultsDomains],
        };
        // top_sync = 30 (Flag), provider_base = 70 (ProviderValue) → 70 > 30 → true
        assert!(result.has_pending_high_priority());
    }

    fn empty_generator_spec() -> Arc<crate::specs::GeneratorSpec> {
        Arc::new(crate::specs::GeneratorSpec {
            generator_type: None,
            script: None,
            script_template: None,
            transforms: vec![],
            cache: None,
            requires_js: false,
            js_source: None,
            corrected_in: None,
            template: None,
        })
    }

    #[test]
    fn has_pending_high_priority_true_when_script_pending_and_flags_only_in_sync() {
        let result = SyncResult {
            suggestions: vec![Suggestion {
                kind: SuggestionKind::Flag,
                priority: None,
                ..Default::default()
            }],
            script_generators: vec![empty_generator_spec()],
            git_generators: vec![],
            provider_generators: vec![],
        };
        // top_sync = 30 (Flag), provider_base = 70 (script → ProviderValue) → 70 > 30 → true
        assert!(result.has_pending_high_priority());
    }

    #[test]
    fn has_pending_high_priority_false_when_script_pending_but_sync_outranks() {
        let result = SyncResult {
            suggestions: vec![Suggestion {
                kind: SuggestionKind::Subcommand,
                priority: None,
                ..Default::default()
            }],
            script_generators: vec![empty_generator_spec()],
            git_generators: vec![],
            provider_generators: vec![],
        };
        // top_sync = 70 (Subcommand), provider_base = 70 → NOT strictly greater → false
        assert!(!result.has_pending_high_priority());
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

    #[doc(hidden)]
    pub fn with_aliases(self, map: std::collections::HashMap<String, Vec<String>>) -> Self {
        self.alias_map.install(map);
        self
    }

    #[doc(hidden)]
    pub fn with_ssh_host_cache_path(mut self, path: std::path::PathBuf) -> Self {
        self.ssh_host_cache = Some(SshHostCache::new(path));
        self
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
                            kind: SuggestionKind::Command,
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

    /// Resolve native providers asynchronously. Mirrors `resolve_git`:
    /// per-kind failures are downgraded to `tracing::warn!` + empty vec
    /// so a single slow or broken provider cannot block the rest of the
    /// pool. Empty-query case skips `fuzzy::rank` to preserve the raw
    /// kind-ordering for the handler's eventual re-rank (same rationale
    /// as `resolve_git`).
    ///
    /// CONTRACT: a per-kind `Err` MUST be logged via `tracing::warn!` and
    /// the loop MUST continue — do NOT rewrite this loop with `?` or any
    /// other short-circuit. One failing provider must not block sibling
    /// providers; the top-level `Result` is reserved for truly fatal
    /// conditions (none today). Providers are expected to absorb their
    /// own transient failures into `Ok(vec![])`, but this loop is the
    /// final backstop against any future provider that surfaces an
    /// `Err`.
    pub async fn resolve_providers(
        &self,
        kinds: &[ProviderKind],
        ctx: &ProviderCtx,
        query: &str,
    ) -> Result<Vec<Suggestion>> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        let mut all = Vec::new();
        for &kind in kinds {
            match providers::resolve(kind, ctx).await {
                Ok(suggestions) => all.extend(suggestions),
                Err(e) => {
                    tracing::warn!(provider = ?kind, "provider failed: {e}");
                }
            }
        }
        if query.is_empty() {
            return Ok(all);
        }
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
        if ctx.command.is_none() {
            return Ok(Vec::new());
        }
        let Some(spec) = self.spec_for_ctx(ctx) else {
            return Ok(Vec::new());
        };
        let resolve_ctx = self.resolve_ctx_for_spec_walk(ctx);
        let resolution = specs::resolve_spec(spec, resolve_ctx.as_ref());
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
        use crate::context::{classify, ClassifyInput, Context};

        let spec_matched = self.spec_for_ctx(ctx).is_some();
        let context = classify(ClassifyInput {
            current_word: &ctx.current_word,
            in_redirect: ctx.in_redirect,
            word_index: ctx.word_index,
            spec_matched,
        });

        match context {
            Context::CommandPosition => Ok(self.suggest_command_position(ctx, cwd, buffer)),
            Context::Redirect => Ok(self.suggest_redirect(ctx, cwd, buffer)),
            Context::PathPrefix => {
                // PathPrefix is the explicit user-typed escape hatch — only
                // filesystem candidates run, regardless of spec content.
                // Env-var (`$VAR`) and ssh-host injections are deliberately
                // absent: PathPrefix words start with `./`, `../`, `/`, or
                // `~/` — none of those prefixes can collide with `$VAR` or
                // an SSH host token, so neither augmentation has anything
                // to add here.
                Ok(self.suggest_filesystem_fallback(ctx, cwd, buffer, Vec::new(), "path"))
            }
            Context::FlagPrefix => Ok(self.suggest_flag_prefix(ctx, cwd, buffer)),
            Context::SpecArg => {
                // Env vars and ssh hosts are situational injections that augment
                // (but do not replace) spec results — they're allowed inside
                // SpecArg context.
                let mut candidates = Vec::new();
                self.extend_with_env_vars(ctx, cwd, &mut candidates);
                self.extend_with_ssh_hosts(ctx, &mut candidates);
                match self.try_suggest_from_spec(ctx, cwd, buffer, candidates) {
                    Ok(result) => Ok(result),
                    Err(_) => unreachable!(
                        "spec_for_ctx returned Some in classify but try_suggest_from_spec \
                         returned Err — alias_map / spec_store invariant violated"
                    ),
                }
            }
            Context::UnspeccedArg => {
                // No spec at all — fall back to the historical behavior:
                // filesystem + history + situational injections.
                let mut candidates = Vec::new();
                self.extend_with_env_vars(ctx, cwd, &mut candidates);
                self.extend_with_ssh_hosts(ctx, &mut candidates);
                Ok(self.suggest_filesystem_fallback(ctx, cwd, buffer, candidates, "fallback"))
            }
        }
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
            suggestions: self.rank_with_history(ctx, cwd, buffer, candidates, true),
            script_generators: Vec::new(),
            git_generators: Vec::new(),
            provider_generators: Vec::new(),
        }
    }

    /// Complete a flag-prefixed token (`-` or `--`). Returns spec-declared
    /// flags + subcommands only — never filesystem, never history.
    fn suggest_flag_prefix(&self, ctx: &CommandContext, cwd: &Path, buffer: &str) -> SyncResult {
        let mut candidates = Vec::new();
        if let Some(spec) = self.spec_for_ctx(ctx) {
            // Walk the alias target's spec subtree, not the literal alias name's.
            let resolve_ctx = self.resolve_ctx_for_spec_walk(ctx);
            let resolution = specs::resolve_spec(spec, resolve_ctx.as_ref());
            candidates.extend(resolution.subcommands);
            candidates.extend(resolution.options);
        }
        SyncResult {
            suggestions: self.rank_with_history(ctx, cwd, buffer, candidates, false),
            script_generators: Vec::new(),
            git_generators: Vec::new(),
            provider_generators: Vec::new(),
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
            suggestions: self.rank_with_history(ctx, cwd, buffer, candidates, true),
            script_generators: Vec::new(),
            git_generators: Vec::new(),
            provider_generators: Vec::new(),
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
        if ctx.command.is_none() {
            return;
        }
        // Use the alias's resolved head so `alias dev=ssh` still triggers ssh-host injection.
        let resolved_cmd: String = match expand_alias_for_spec(ctx, &self.alias_map) {
            Some(exp) => exp.resolved_command.into_owned(),
            None => return,
        };
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

    /// Pivot ctx onto the alias target so spec walks land in the right subcommand.
    fn resolve_ctx_for_spec_walk<'a>(
        &self,
        ctx: &'a CommandContext,
    ) -> std::borrow::Cow<'a, CommandContext> {
        match expand_alias_for_spec(ctx, &self.alias_map) {
            Some(exp) if exp.aliased => {
                let synthetic = CommandContext {
                    command: Some(exp.resolved_command.into_owned()),
                    args: exp.effective_args.into_owned(),
                    ..ctx.clone()
                };
                std::borrow::Cow::Owned(synthetic)
            }
            _ => std::borrow::Cow::Borrowed(ctx),
        }
    }

    /// Resolve the alias-aware spec for this command context, if any.
    /// Centralizes the alias lookup + spec_store probe so callers don't
    /// repeat it.
    fn spec_for_ctx(&self, ctx: &CommandContext) -> Option<&specs::CompletionSpec> {
        if !self.providers_specs {
            return None;
        }
        // expand_alias_for_spec covers both aliased and unaliased paths in one lookup.
        let expanded = expand_alias_for_spec(ctx, &self.alias_map)?;
        self.spec_store.get(expanded.resolved_command.as_ref())
    }

    /// Look up the spec for `ctx.command_name` and append its synchronous
    /// completions (subcommands, options, templates, env-var/SSH-host
    /// injections) to the candidate set.
    ///
    /// By construction this is invoked only from the `Context::SpecArg` arm
    /// of `suggest_sync`, which has already verified
    /// `spec_for_ctx(...).is_some()` via the classifier. The `Err(candidates)`
    /// arm therefore signals an internal invariant violation (`alias_map` and
    /// `spec_store` mutation between classify and dispatch) and is converted
    /// to `unreachable!` by the dispatcher.
    fn try_suggest_from_spec(
        &self,
        ctx: &CommandContext,
        cwd: &Path,
        buffer: &str,
        mut candidates: Vec<Suggestion>,
    ) -> std::result::Result<SyncResult, Vec<Suggestion>> {
        let Some(spec) = self.spec_for_ctx(ctx) else {
            return Err(candidates);
        };

        // Synthetic ctx: spec walk uses the expansion; ranking/history stay on the literal buffer.
        let resolve_ctx = self.resolve_ctx_for_spec_walk(ctx);

        let specs::SpecResolution {
            subcommands,
            options,
            native_generators,
            provider_generators,
            script_generators,
            wants_filepaths,
            wants_folders_only,
            preceding_flag_has_args,
            past_double_dash,
            ..
        } = specs::resolve_spec(spec, resolve_ctx.as_ref());

        let git_generators = self.git_generators_from(&native_generators);

        // Suppress subcommands/options when:
        // 1. The preceding flag takes an argument (e.g. `curl -o <TAB>`)
        // 2. We're past `--` (end-of-flags separator) — only positional args
        let suppress_commands = preceding_flag_has_args || past_double_dash;

        if !suppress_commands {
            candidates.extend(subcommands);
            candidates.extend(options);
        }

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

        let suggestions = self.rank_with_history(ctx, cwd, buffer, candidates, true);

        Ok(SyncResult {
            suggestions,
            script_generators,
            git_generators,
            provider_generators,
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
            // When typing a `../`-prefixed word, offer one more level of parent
            // navigation (e.g. `../../`) so the user can chain upward without
            // switching context. This applies the trailing-`../` portion of
            // the parent-nav logic from `extend_with_folders`. The empty-word
            // case (`cd <TAB>` injecting `../`) is intentionally NOT mirrored
            // here — that path goes through SpecArg context, never reaches
            // this fallback.
            if ctx.current_word.ends_with("../") {
                let parent_text = format!("{}../", &ctx.current_word);
                let effective = cwd.join(&ctx.current_word);
                let at_boundary = effective.canonicalize().ok().is_none_or(|resolved| {
                    resolved == Path::new("/")
                        || std::env::var("HOME")
                            .ok()
                            .is_some_and(|h| resolved == Path::new(&h))
                });
                if !at_boundary {
                    candidates.push(Suggestion {
                        text: parent_text,
                        description: Some("Parent directory".to_string()),
                        kind: SuggestionKind::Directory,
                        source: SuggestionSource::Filesystem,
                        ..Default::default()
                    });
                }
            }
            match self.filesystem_provider.provide(ctx, cwd) {
                Ok(fs) => candidates.extend(fs),
                Err(e) => tracing::warn!("filesystem provider error ({label}): {e}"),
            }
        }
        SyncResult {
            suggestions: self.rank_with_history(ctx, cwd, buffer, candidates, true),
            script_generators: Vec::new(),
            git_generators: Vec::new(),
            provider_generators: Vec::new(),
        }
    }

    /// Rank main candidates with current_word, then separately rank history
    /// candidates with the full buffer, and append history results at the end.
    /// All suggestions receive a frecency bonus so frequently/recently accepted
    /// completions sort higher. When `include_history` is false, history is
    /// skipped entirely — used by `suggest_flag_prefix`, where the user has
    /// explicitly typed a flag dash and history entries (full command lines)
    /// would create irrelevant noise.
    fn rank_with_history(
        &self,
        ctx: &CommandContext,
        cwd: &Path,
        buffer: &str,
        candidates: Vec<Suggestion>,
        include_history: bool,
    ) -> Vec<Suggestion> {
        let mut results = fuzzy::rank(&ctx.current_word, candidates, self.max_results);

        // History doesn't belong in redirect context — user expects filenames, not commands
        if include_history && self.max_history_results > 0 && !ctx.in_redirect {
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

        // Apply frecency boost to ALL suggestions, then re-sort by
        // (history-partition, score-desc, priority-desc, alpha).
        //
        // The explicit `a_hist` / `b_hist` partition is retained on purpose
        // even though `priority::effective(History) == 10` would normally
        // sink history to the bottom. Frecency can boost a heavily-used
        // history entry's `score` well above non-history items, and
        // because score is the primary sort key, a boosted history match
        // could otherwise outrank domain content on the same query. The
        // partition guarantees history never outranks non-history
        // regardless of how aggressive frecency gets.
        self.frecency_db
            .boost_scores(&mut results, ctx.command.as_deref());
        results.sort_by(|a, b| {
            let a_hist = a.source == SuggestionSource::History;
            let b_hist = b.source == SuggestionSource::History;
            a_hist
                .cmp(&b_hist)
                .then_with(|| b.score.cmp(&a.score))
                .then_with(|| priority::effective(b).cmp(&priority::effective(a)))
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
    fn test_git_checkout_dispatches_ref_generators_in_arg_position() {
        // SpecArg dispatches generators in parallel with sync flags; priority
        // sort lands branches above flags once they arrive.
        let engine = make_engine();
        let ctx = make_ctx(Some("git"), vec!["checkout"], "", 2);
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "git checkout ")
            .unwrap();

        assert!(
            results
                .git_generators
                .contains(&crate::git::GitQueryKind::Branches),
            "git checkout should dispatch branch generator: {results:?}"
        );
        assert!(
            results
                .git_generators
                .contains(&crate::git::GitQueryKind::Tags),
            "git checkout should dispatch tag generator: {results:?}"
        );
    }

    #[test]
    fn test_git_checkout_includes_history_in_arg_position() {
        // SpecArg context always includes history (rank_with_history true).
        // A discriminating current_word ("main") is used so the spec/fs
        // candidates fuzzy-filter down and history can fit within
        // max_results — an empty current_word floods the cap with flags
        // and folders before the history append runs.
        let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap().store;
        let history = HistoryProvider::from_entries(vec![
            "git checkout main".into(),
            "git checkout -b feature".into(),
            "git checkout demo".into(),
        ]);
        let commands = CommandsProvider::from_list(vec!["git".into()]);
        let engine = SuggestionEngine::with_providers(spec_store, history, commands);

        let ctx = make_ctx(Some("git"), vec!["checkout"], "main", 2);
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "git checkout main")
            .unwrap();

        // Presence is locked in here; ordering against incoming async
        // branches is covered by the priority-sort tests above.
        assert!(
            results
                .suggestions
                .iter()
                .any(|s| s.source == SuggestionSource::History),
            "SpecArg must include history matches: {:?}",
            results
                .suggestions
                .iter()
                .map(|s| (&s.text, &s.source))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_git_checkout_still_offers_filesystem_when_refs_pending() {
        // `git checkout <file>` is a valid restore-file invocation. Deferring
        // to git refs must NOT swallow filesystem completions — the user might
        // be mid-word on a filename.
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("Makefile"), "").unwrap();
        std::fs::write(tmp.path().join("README.md"), "").unwrap();

        let engine = make_engine();
        let ctx = make_ctx(Some("git"), vec!["checkout"], "", 2);
        let results = engine
            .suggest_sync(&ctx, tmp.path(), "git checkout ")
            .unwrap();

        assert!(
            results
                .suggestions
                .iter()
                .any(|s| s.text == "Makefile" || s.text == "README.md"),
            "filesystem completions must still appear while git ref generators \
             are pending so `git checkout <file>` keeps working: {:?}",
            results.suggestions,
        );
    }

    #[test]
    fn test_git_checkout_with_flag_prefix_still_shows_flags() {
        // FlagPrefix context dispatches to suggest_flag_prefix which returns
        // spec-declared flags and subcommands only — no filesystem, no git
        // generators. When the user types `-` they have signalled they want
        // flags; git ref generators are not dispatched in this path (they're
        // dispatched when the user is in SpecArg context with an empty token).
        let engine = make_engine();
        let ctx = make_ctx(Some("git"), vec!["checkout"], "-", 2);
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "git checkout -")
            .unwrap();

        assert!(
            results
                .suggestions
                .iter()
                .any(|s| s.kind == SuggestionKind::Flag),
            "flags must appear when current_word starts with '-': {:?}",
            results.suggestions,
        );
        // FlagPrefix no longer dispatches git generators — flags are the
        // explicit intent, and git refs are an async concern for SpecArg.
        assert!(
            results.git_generators.is_empty(),
            "FlagPrefix should not dispatch git generators: {results:?}"
        );
    }

    #[test]
    fn test_git_checkout_with_path_like_word_does_not_defer_to_refs() {
        // Path-prefixed words (starting with `./`, `../`, `~/`) route to the
        // PathPrefix context which calls suggest_filesystem_fallback with
        // include_history=true. Words that embed `/` but lack those prefixes
        // (e.g. `src/main`) route to SpecArg where history is always included
        // (rank_with_history is called with true). Either way, history must
        // not be suppressed when the user has signalled a path — otherwise
        // `git checkout ./foo` etc. would lose matching history entries.
        //
        // Filesystem is disabled on the engine so the assertion targets the
        // `include_history` branch directly — otherwise real-world filesystem
        // entries crowd out history via `max_results` saturation.
        let path_markers = ["./", "../src", "~/proj", "src/main"];

        for marker in path_markers {
            let tmp = tempfile::TempDir::new().unwrap();
            let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap().store;
            // History entry is crafted so the buffer fuzzy-matches it,
            // proving history candidates ARE reachable on this path.
            let history_entry = format!("git checkout {marker}");
            let history = HistoryProvider::from_entries(vec![history_entry.clone()]);
            let commands = CommandsProvider::from_list(vec!["git".into()]);
            let mut engine = SuggestionEngine::with_providers(spec_store, history, commands);
            engine.providers_filesystem = false;

            let ctx = make_ctx(Some("git"), vec!["checkout"], marker, 2);
            let buffer = format!("git checkout {marker}");
            let results = engine.suggest_sync(&ctx, tmp.path(), &buffer).unwrap();

            assert!(
                results
                    .suggestions
                    .iter()
                    .any(|s| s.source == SuggestionSource::History),
                "path-like word {marker:?} must NOT suppress history: {:?}",
                results.suggestions,
            );
        }
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
    fn test_path_prefix_dispatches_via_classifier() {
        // Genuinely exercises the PathPrefix Context branch — `./foo` starts
        // with `./` so `has_path_prefix` returns true and the classifier
        // routes to PathPrefix instead of UnspeccedArg.
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("foo")).unwrap();
        std::fs::write(tmp.path().join("foo/bar.txt"), "").unwrap();
        let ctx = make_ctx(Some("cat"), vec![], "./foo", 1);
        let results = engine.suggest_sync(&ctx, tmp.path(), "cat ./foo").unwrap();
        assert!(
            results.iter().any(|s| s.text.contains("foo")),
            "PathPrefix dispatch should yield filesystem entries: {results:?}"
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
    fn test_path_prefix_chains_parent_dir_for_unspecced_command() {
        use crate::context::{classify, ClassifyInput, Context};
        // PathPrefix on an unspecced command should still offer the chained
        // `../../` when the user is one level deep into the working tree.
        let engine = make_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        let sub = tmp.path().join("aaa").join("bbb");
        std::fs::create_dir_all(&sub).unwrap();
        let ctx = make_ctx(Some("unknown_cmd"), vec![], "../", 1);
        assert_eq!(
            classify(ClassifyInput {
                current_word: "../",
                in_redirect: false,
                word_index: 1,
                spec_matched: false,
            }),
            Context::PathPrefix
        );
        let results = engine.suggest_sync(&ctx, &sub, "unknown_cmd ../").unwrap();
        assert!(
            results.iter().any(|s| s.text == "../../"),
            "PathPrefix should chain parent dir on unspecced commands: {results:?}"
        );
    }

    #[test]
    fn test_unspecced_path_prefix_no_chain_at_root() {
        // Root has no parent — `../` chaining must not appear.
        let engine = make_engine();
        let ctx = make_ctx(Some("unknown_cmd"), vec![], "../", 1);
        let results = engine
            .suggest_sync(&ctx, Path::new("/"), "unknown_cmd ../")
            .unwrap();
        assert!(
            !results
                .iter()
                .any(|s| s.text == "../" || s.text == "../../"),
            "../ chaining should not appear at root: {results:?}"
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
        // Uses a made-up `myapp` spec with only plain subcommands — no
        // filepath args, no generators. This avoids colliding with native
        // generators (e.g. `git push` now dispatches to the `git_remotes`
        // provider, which triggers the defer-to-git-refs history-suppression
        // path introduced in 0e10f7c) and keeps filesystem fallback from
        // flooding `max_results` before history is appended.
        let spec_json = r#"{
            "name": "myapp",
            "subcommands": [
                {"name": "deploy", "subcommands": [
                    {"name": "production"},
                    {"name": "staging"}
                ]},
                {"name": "build"}
            ]
        }"#;
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("myapp.json"), spec_json).unwrap();
        let spec_store = SpecStore::load_from_dir(dir.path()).unwrap().store;
        let history = HistoryProvider::from_entries(vec![
            "myapp deploy production".into(),
            "myapp build release".into(),
        ]);
        let commands = CommandsProvider::from_list(vec!["myapp".into()]);
        let engine = SuggestionEngine::with_providers(spec_store, history, commands);

        let ctx = make_ctx(Some("myapp"), vec!["deploy"], "", 2);
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "myapp deploy ")
            .unwrap();
        let hist: Vec<_> = results
            .iter()
            .filter(|s| s.source == crate::types::SuggestionSource::History)
            .collect();
        assert!(
            hist.iter().any(|s| s.text == "myapp deploy production"),
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
    async fn test_suggest_dynamic_script_generator_without_transforms() {
        // Covers the `if transforms.is_empty()` branch in
        // SuggestionEngine::suggest_dynamic (~engine.rs:284-295) — the only
        // path where a spec generator without a `transforms` field flows
        // through. The branch explicitly sets kind=Command + source=Script;
        // without this test, refactors that drop the filtering or change
        // the kind/source pair would ship silently. Note the spec has NO
        // "transforms" field, unlike test_suggest_dynamic_with_script_generator.
        let spec_json = r#"{
            "name": "test-dynamic-no-transforms",
            "args": [{
                "generators": [{
                    "script": ["printf", "alpha\nbeta\n\n"]
                }]
            }]
        }"#;
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("test-dynamic-no-transforms.json"),
            spec_json,
        )
        .unwrap();

        let dirs = vec![dir.path().to_path_buf()];
        let engine = SuggestionEngine::new(&dirs).unwrap();
        let ctx = make_ctx(Some("test-dynamic-no-transforms"), vec![], "", 1);
        let results = engine
            .suggest_dynamic(&ctx, Path::new("/tmp"), 5000)
            .await
            .unwrap();
        // Default branch filters empty lines, so the trailing blank line
        // from "alpha\nbeta\n\n" must be dropped.
        assert_eq!(
            results.len(),
            2,
            "empty line should be filtered: {results:?}"
        );
        assert_eq!(results[0].text, "alpha");
        assert_eq!(results[1].text, "beta");
        // Pin kind/source on the default branch so refactors can't silently
        // flip to Suggestion::default() (ProviderValue).
        assert!(
            results
                .iter()
                .all(|s| s.kind == SuggestionKind::Command && s.source == SuggestionSource::Script),
            "all results must be kind=Command, source=Script: {results:?}"
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
    async fn test_resolve_providers_empty_slice() {
        // An empty `kinds` slice must no-op cleanly — empty Vec and no
        // panic — for both the empty-query and non-empty-query paths.
        // Guards the empty-kinds shortcut at the top of
        // `resolve_providers` against accidental removal, which would
        // otherwise make the method pay a `fuzzy::rank` roundtrip on
        // every call-site that passes an empty slice (a common case
        // when a resolved spec has no provider generators).
        let engine = make_engine();
        let ctx = crate::providers::ProviderCtx {
            cwd: Path::new("/tmp").to_path_buf(),
            env: std::sync::Arc::new(std::collections::HashMap::new()),
            current_token: String::new(),
        };
        let empty_query = engine.resolve_providers(&[], &ctx, "").await.unwrap();
        assert!(empty_query.is_empty());
        let non_empty_query = engine.resolve_providers(&[], &ctx, "foo").await.unwrap();
        assert!(non_empty_query.is_empty());
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
            corrected_in: None,
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
            corrected_in: None,
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
    fn test_priority_tiebreaker_with_equal_boosted_scores() {
        // When two suggestions have the same score after frecency boost,
        // effective priority should break the tie (GitBranch > Subcommand > Flag).
        let engine = make_engine();
        let ctx = make_ctx(Some("git"), vec![], "ch", 1);
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "git ch")
            .unwrap();

        let non_hist: Vec<_> = results
            .iter()
            .filter(|s| s.source != SuggestionSource::History)
            .collect();

        // For any adjacent pair with equal scores, verify priority ordering (descending)
        for pair in non_hist.windows(2) {
            if pair[0].score == pair[1].score {
                assert!(
                    priority::effective(pair[0]) >= priority::effective(pair[1]),
                    "equal-score items should be ordered by priority desc: {:?} (pri={}) before {:?} (pri={})",
                    pair[0].text,
                    priority::effective(pair[0]).get(),
                    pair[1].text,
                    priority::effective(pair[1]).get()
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

    // ---- helpers for Context-dispatch tests ----

    /// Synthesise a `CommandContext` from a raw buffer string.
    ///
    /// Splits on spaces; trailing space means `current_word` is `""` at
    /// `word_index == token_count`. Leading token is the command, remaining
    /// tokens before the last are `args`, last token is `current_word`.
    fn command_context_with(buffer: &str) -> CommandContext {
        // Tokenise, preserving trailing empty slot for "ends with space".
        let ends_with_space = buffer.ends_with(' ');
        let tokens: Vec<&str> = buffer.split_whitespace().collect();
        if tokens.is_empty() {
            return make_ctx(None, vec![], "", 0);
        }
        let command = tokens[0];
        if tokens.len() == 1 && !ends_with_space {
            // "git" — still typing the command
            return make_ctx(None, vec![], command, 0);
        }
        let (args_slice, current_word) = if ends_with_space {
            // All tokens are completed args; current_word is blank.
            (&tokens[1..], "")
        } else {
            // Last token is the word being typed.
            (&tokens[1..tokens.len() - 1], *tokens.last().unwrap())
        };
        let word_index = 1 + args_slice.len();
        make_ctx(Some(command), args_slice.to_vec(), current_word, word_index)
    }

    // ---- Context-dispatch contract tests ----

    #[test]
    fn suggest_sync_path_prefix_returns_filesystem_only() {
        let engine = make_engine();
        let ctx = command_context_with("git checkout ./");
        let result = engine
            .suggest_sync(&ctx, std::path::Path::new("/tmp"), "git checkout ./")
            .unwrap();
        assert!(
            result.suggestions.iter().all(|s| matches!(
                s.kind,
                crate::types::SuggestionKind::FilePath | crate::types::SuggestionKind::Directory
            )),
            "PathPrefix context should yield only filesystem suggestions, got {:?}",
            result
                .suggestions
                .iter()
                .map(|s| &s.kind)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn suggest_sync_flag_prefix_returns_flags_and_subcommands_only() {
        let engine = make_engine();
        let ctx = command_context_with("git checkout --");
        let result = engine
            .suggest_sync(&ctx, std::path::Path::new("/tmp"), "git checkout --")
            .unwrap();
        assert!(
            result.suggestions.iter().all(|s| matches!(
                s.kind,
                crate::types::SuggestionKind::Flag | crate::types::SuggestionKind::Subcommand
            )),
            "FlagPrefix context should yield only Flag/Subcommand suggestions, got {:?}",
            result
                .suggestions
                .iter()
                .map(|s| &s.kind)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn suggest_sync_spec_arg_does_not_inject_filesystem_when_spec_omits_template() {
        // Use a spec with NO template at the positional arg (e.g. cargo run).
        let engine = make_engine();
        let ctx = command_context_with("cargo run ");
        let result = engine
            .suggest_sync(&ctx, std::path::Path::new("/tmp"), "cargo run ")
            .unwrap();
        let any_fs = result.suggestions.iter().any(|s| {
            matches!(
                s.kind,
                crate::types::SuggestionKind::FilePath | crate::types::SuggestionKind::Directory
            )
        });
        assert!(
            !any_fs,
            "spec without template should NOT inject fs, got {:?}",
            result
                .suggestions
                .iter()
                .map(|s| (&s.text, &s.kind))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn git_archive_format_returns_tar_zip() {
        let engine = make_engine();
        let ctx = CommandContext {
            command: Some("git".into()),
            args: vec!["archive".into(), "--format=".into()],
            current_word: String::new(),
            word_index: 3,
            is_flag: false,
            is_long_flag: false,
            // `find_option` strips the `=value` suffix internally, so passing
            // `--format=` (with trailing `=`) or `--format` both resolve to the
            // archive subcommand's `--format` option.
            preceding_flag: Some("--format=".into()),
            in_pipe: false,
            in_redirect: false,
            quote_state: QuoteState::None,
            is_first_segment: true,
        };
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "git archive --format=")
            .unwrap();
        let texts: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
        assert!(texts.contains(&"tar"), "expected `tar` in {texts:?}");
        assert!(texts.contains(&"zip"), "expected `zip` in {texts:?}");
    }
}
