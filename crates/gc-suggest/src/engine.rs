use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use gc_buffer::CommandContext;
use gc_jsrt::{JsDiagnosticCode, JsRuntimeOutputPayload};
use tokio::sync::Semaphore;

use crate::alias::AliasStore;
use crate::alias_expand::expand_alias_for_spec;
use crate::cache::{hash_env, hash_js_source, CacheKey, GeneratorCache};
use crate::commands::CommandsProvider;
use crate::env::EnvProvider;
use crate::filesystem::FilesystemProvider;
use crate::frecency::FrecencyDb;
use crate::fuzzy;
use crate::git;
use crate::history::{HistoryProvider, DEFAULT_MAX_HISTORY_ENTRIES};
use crate::js_runtime::{JsExecContext, JsRuntimeAdapter};
use crate::priority;
use crate::provider::Provider;
use crate::providers::{self, ProviderCtx, ProviderKind, ProviderResolution};
use crate::script::{run_script_with_env, substitute_template};
use crate::shell_runner::EngineShellRunner;
use crate::specs::{self, GeneratorSpec, JsRuntimeKind, JsRuntimeSpec, SpecStore};
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

/// Consecutive token-only failures (timeouts or other hard runtime errors)
/// allowed before a generator is skipped for the rest of the engine
/// process lifetime.
const TOKEN_ONLY_DEMOTE_AFTER_FAILURES: u8 = 2;

#[derive(Debug, Default)]
struct TokenOnlyDemotionState {
    consecutive_failures: Mutex<HashMap<String, u8>>,
}

impl TokenOnlyDemotionState {
    /// Acquire the failures map, recovering from a poisoned mutex by
    /// wiping state, clearing the poison flag, and emitting a single
    /// `tracing::warn!`. The contract is: callers NEVER see a
    /// `PoisonError` and NEVER observe a partially updated map across
    /// panics, and after one poison event the mutex is fully restored —
    /// subsequent calls take the `Ok` arm, so the warn fires exactly once
    /// per poison event rather than on every lock for the rest of the
    /// process lifetime.
    ///
    /// A poisoned mutex means a panic interrupted a mutation; the
    /// partially-written map may have stale or torn counts. Clearing is
    /// safer than reusing — demotion counts re-accrue on the next real
    /// timeout/failure.
    fn lock_failures(&self) -> std::sync::MutexGuard<'_, HashMap<String, u8>> {
        match self.consecutive_failures.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                tracing::warn!(
                    "token_only demotion mutex was poisoned; clearing state and continuing"
                );
                let mut g = poisoned.into_inner();
                *g = Default::default();
                self.consecutive_failures.clear_poison();
                g
            }
        }
    }

    fn is_demoted(&self, generator_id: &str) -> bool {
        let failures = self.lock_failures();
        failures.get(generator_id).copied().unwrap_or(0) >= TOKEN_ONLY_DEMOTE_AFTER_FAILURES
    }

    fn record_timeout(&self, generator_id: &str) -> u8 {
        let mut failures = self.lock_failures();
        let count = failures.entry(generator_id.to_string()).or_insert(0);
        *count = count.saturating_add(1);
        *count
    }

    /// Bumps the consecutive-failure counter for a non-timeout hard error
    /// (exception, memory exhaustion, oversized output, etc.). Returned
    /// count uses the same threshold as [`Self::record_timeout`] for
    /// demotion logging.
    fn record_failure(&self, generator_id: &str) -> u8 {
        self.record_timeout(generator_id)
    }

    /// Clear the consecutive-failure counter after a real success — JS
    /// evaluated to a non-empty Suggestions payload.
    ///
    /// Only [`JsDiagnosticCode::Timeout`] (via [`Self::record_timeout`])
    /// and [`JsDiagnosticCode::Exception`] /
    /// [`JsDiagnosticCode::MemoryExceeded`] /
    /// [`JsDiagnosticCode::OversizedOutput`] (via
    /// [`Self::record_failure`]) bump the counter; only a non-empty
    /// `Suggestions` payload resets it. Every other diagnostic
    /// (`EmptyOutput`, `InvalidShape`, `UnsupportedHostApi`,
    /// `UnsupportedApi`, `ShellCommandStringDenied`,
    /// `ShellCommandLimitExceeded`, `ShellCommandFailed`, `InvalidArgv`)
    /// leaves the counter untouched so a recovery still requires real
    /// output. `UnsupportedHostApi` is load-bearing for the token_only
    /// design — token_only sources that touch a host API surface that
    /// diagnostic and we deliberately do not treat that as a failure.
    fn record_success(&self, generator_id: &str) {
        let mut failures = self.lock_failures();
        failures.remove(generator_id);
    }
}

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
    /// Carries pre-resolved [`ProviderResolution`] entries — each pairs
    /// a `ProviderKind` with the `Arc<BTreeMap<String, String>>` of
    /// generator-spec `params` declared on the source `GeneratorSpec`.
    /// The engine threads those params into [`ProviderCtx::params`] at
    /// dispatch time so spec-driven providers can read them without a
    /// trait-shape change.
    pub provider_generators: Vec<ProviderResolution>,
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
            provider_generators: vec![ProviderKind::DefaultsDomains.into()],
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
            lowered_from_requires_js: false,
            static_extracted_subprocess: false,
            requires_js: false,
            js_source: None,
            js_runtime: None,
            corrected_in: None,
            template: None,
            params: std::collections::BTreeMap::new(),
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
    spec_store: Arc<SpecStore>,
    filesystem_provider: FilesystemProvider,
    history_provider: HistoryProvider,
    commands_provider: CommandsProvider,
    env_provider: EnvProvider,
    ssh_host_cache: Option<SshHostCache>,
    alias_map: AliasStore,
    generator_cache: Arc<GeneratorCache>,
    /// Lazily-spawned QuickJS worker. Only paid for when a `requires_js`
    /// generator (`post_process`, `script_function`, `custom`, or
    /// `token_only`) actually fires. Held in an `Arc` so per-generator
    /// tasks can share it without taking `&self` references across
    /// `tokio::spawn`.
    js_runtime: Arc<JsRuntimeAdapter>,
    token_only_demotion_state: Arc<TokenOnlyDemotionState>,
    frecency_db: FrecencyDb,
    max_results: usize,
    max_history_results: usize,
    providers_commands: bool,
    providers_filesystem: bool,
    providers_specs: bool,
    providers_git: bool,
    providers_aws_sdk: bool,
    aws_sdk_fallback_to_cli: bool,
    /// Kill switch for the JS evaluator. When `false`, every JS dispatch
    /// path is bypassed and `requires_js` generators behave as if their
    /// `js_runtime` were missing — they're dropped from the generator
    /// pool. Mirrors `ProvidersConfig::js_runtime` from `gc-config`.
    providers_js_runtime: bool,
}

impl SuggestionEngine {
    pub fn new(spec_dirs: &[PathBuf]) -> Result<Self> {
        Self::new_with_embedded(spec_dirs, spec_dirs.is_empty())
    }

    /// Construct an engine with explicit control over embedded spec fallback.
    ///
    /// `include_embedded = false` preserves `paths.spec_dirs` as an exact
    /// override. `true` is the auto-detected/fallback runtime path.
    pub fn new_with_embedded(spec_dirs: &[PathBuf], include_embedded: bool) -> Result<Self> {
        let result = if include_embedded {
            SpecStore::load_with_embedded(spec_dirs)?
        } else {
            SpecStore::load_from_dirs(spec_dirs)?
        };
        if !result.directory_errors.is_empty() {
            tracing::warn!(
                "{} spec dir(s) failed to scan (run `ghost-complete validate-specs` for details): {}",
                result.directory_errors.len(),
                result.directory_errors.join(", ")
            );
        }
        Ok(Self {
            spec_store: Arc::new(result.store),
            filesystem_provider: FilesystemProvider::new(),
            history_provider: HistoryProvider::load(DEFAULT_MAX_HISTORY_ENTRIES),
            commands_provider: CommandsProvider::from_path_env(),
            env_provider: EnvProvider::new(),
            ssh_host_cache: SshHostCache::default_path(),
            alias_map: AliasStore::load_async(),
            generator_cache: Arc::new(GeneratorCache::new()),
            js_runtime: Arc::new(JsRuntimeAdapter::new()),
            token_only_demotion_state: Arc::new(TokenOnlyDemotionState::default()),
            frecency_db: FrecencyDb::load(),
            max_results: fuzzy::DEFAULT_MAX_RESULTS,
            max_history_results: 5,
            providers_commands: true,
            providers_filesystem: true,
            providers_specs: true,
            providers_git: true,
            providers_aws_sdk: false,
            aws_sdk_fallback_to_cli: true,
            providers_js_runtime: true,
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
        js_runtime: bool,
    ) -> Self {
        self.max_results = max_results;
        self.max_history_results = max_history_results;
        self.providers_commands = commands;
        self.providers_filesystem = filesystem;
        self.providers_specs = specs;
        self.providers_git = git;
        self.providers_js_runtime = js_runtime;
        // Reload history only if enabled
        if max_history_results > 0 {
            self.history_provider = HistoryProvider::load(DEFAULT_MAX_HISTORY_ENTRIES);
        } else {
            self.history_provider = HistoryProvider::from_entries(vec![]);
        }
        self
    }

    pub fn with_aws_sdk_config(mut self, enabled: bool, fallback_to_cli: bool) -> Self {
        self.providers_aws_sdk = enabled;
        self.aws_sdk_fallback_to_cli = fallback_to_cli;
        self
    }

    /// Test/bench constructor — inject providers directly for deterministic setup.
    pub fn with_providers(
        spec_store: SpecStore,
        history_provider: HistoryProvider,
        commands_provider: CommandsProvider,
    ) -> Self {
        Self {
            spec_store: Arc::new(spec_store),
            filesystem_provider: FilesystemProvider::new(),
            history_provider,
            commands_provider,
            env_provider: EnvProvider::new(),
            ssh_host_cache: SshHostCache::default_path(),
            alias_map: AliasStore::empty(),
            generator_cache: Arc::new(GeneratorCache::new()),
            js_runtime: Arc::new(JsRuntimeAdapter::new()),
            token_only_demotion_state: Arc::new(TokenOnlyDemotionState::default()),
            frecency_db: FrecencyDb::empty(),
            max_results: fuzzy::DEFAULT_MAX_RESULTS,
            max_history_results: 5,
            providers_commands: true,
            providers_filesystem: true,
            providers_specs: true,
            providers_git: true,
            providers_aws_sdk: false,
            aws_sdk_fallback_to_cli: true,
            providers_js_runtime: true,
        }
    }

    #[doc(hidden)]
    pub fn with_aliases(self, map: std::collections::HashMap<String, Vec<String>>) -> Self {
        self.alias_map.install(map);
        self
    }

    /// Spawn the cache-eviction sweep task using the provided config.
    /// Returns `None` when eviction is disabled. The caller must keep
    /// the returned guard alive for the lifetime of the engine.
    pub fn spawn_spec_cache_sweep(
        &self,
        cfg: gc_config::SpecCacheConfig,
    ) -> Option<crate::specs::SpecCacheSweep> {
        crate::specs::spawn_spec_cache_sweep(Arc::clone(&self.spec_store), cfg)
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
        self.run_generators_with_env(generators, ctx, cwd, timeout_ms, None)
            .await
    }

    /// Run pre-resolved script generators with an optional shell-reported
    /// environment snapshot. When present, subprocesses and JS host contexts
    /// use this snapshot instead of the proxy process environment.
    pub async fn run_generators_with_env(
        &self,
        generators: &[Arc<specs::GeneratorSpec>],
        ctx: &CommandContext,
        cwd: &Path,
        timeout_ms: u64,
        shell_env: Option<Arc<HashMap<String, String>>>,
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
        let env_hash = shell_env.as_deref().map(hash_env);

        for (idx, gen) in generators.iter().enumerate() {
            // Borrow the inner GeneratorSpec once; we pass references into
            // cache-keying and transform-cloning below just like before the
            // Arc wrapper was added.
            let gen: &specs::GeneratorSpec = gen.as_ref();

            // Routing decisions based on `js_runtime.kind`:
            //   - `PostProcess`: spec carries the script argv; JS runs after.
            //   - `ScriptFunction`: JS evaluates first to produce argv;
            //     then we execute the resolved argv through `run_script`.
            //   - `Custom`: JS produces suggestions directly; no engine-side
            //     script invocation. argv is intentionally left empty.
            //   - `TokenOnly`: JS produces suggestions directly with only
            //     token globals installed. No script argv is needed.
            //   - non-JS: existing path (script + optional transforms).
            //
            // The kill switch (`providers_js_runtime`) drops every
            // requires_js generator when off; non-JS generators are
            // unaffected.
            let js_kind = if gen.requires_js {
                gen.js_runtime.as_ref().map(|rt| rt.kind.clone())
            } else {
                None
            };
            if matches!(
                js_kind,
                Some(JsRuntimeKind::PostProcess)
                    | Some(JsRuntimeKind::ScriptFunction)
                    | Some(JsRuntimeKind::Custom)
                    | Some(JsRuntimeKind::TokenOnly)
            ) && !self.providers_js_runtime
            {
                tracing::info!(
                    spec = %command,
                    generator_index = idx,
                    kind = ?js_kind,
                    "js_runtime kill switch is disabled — skipping requires_js generator"
                );
                continue;
            }

            // PostProcess and non-JS generators resolve argv up front;
            // ScriptFunction computes argv from JS; Custom and TokenOnly
            // skip script execution entirely and produce suggestions
            // directly.
            let argv = resolve_script_argv(gen, ctx);
            let needs_argv = !matches!(
                js_kind,
                Some(JsRuntimeKind::ScriptFunction)
                    | Some(JsRuntimeKind::Custom)
                    | Some(JsRuntimeKind::TokenOnly)
            );
            if needs_argv && argv.is_empty() {
                continue;
            }

            // `js_runtime` is `Arc<JsRuntimeSpec>` at the schema layer so the
            // hot path Arc-clones (cheap pointer bump) instead of deep-copying
            // the embedded JS source on every keystroke. Some corpus
            // generators (notably AWS) carry several KB of source.
            let js_dispatch: Option<Arc<JsRuntimeSpec>> = match (gen.requires_js, &gen.js_runtime) {
                (true, Some(rt)) if rt.kind == JsRuntimeKind::PostProcess => Some(Arc::clone(rt)),
                _ => None,
            };

            // ScriptFunction / Custom dispatch lives on a separate path
            // that does not share the legacy stdout-cache / transform
            // pipeline. We branch off here and let the rest of the loop
            // body handle PostProcess + non-JS.
            if matches!(js_kind, Some(JsRuntimeKind::ScriptFunction)) {
                let rt = match gen.js_runtime.as_ref() {
                    Some(rt) => Arc::clone(rt),
                    None => {
                        tracing::warn!(
                            spec = %command,
                            generator_index = idx,
                            kind = ?js_kind,
                            "requires_js generator with ScriptFunction/Custom kind has no js_runtime metadata — skipping"
                        );
                        continue;
                    }
                };
                let exec_ctx = make_js_exec_context(ctx, cwd, shell_env.as_deref());
                let cmd_name = command.to_string();
                let generator_index = idx;
                let js_runtime = Arc::clone(&self.js_runtime);
                let cwd_buf = cwd.to_path_buf();
                let transforms = gen.transforms.clone();
                let cache = gen.cache.clone();
                let cache_store = Arc::clone(&self.generator_cache);
                let env = shell_env.clone();
                let permit = Arc::clone(&semaphore);
                handles.push(tokio::spawn(async move {
                    let _permit = permit
                        .acquire()
                        .await
                        .map_err(|e| anyhow::anyhow!("semaphore error: {e}"))?;
                    run_script_function_dispatch(
                        rt,
                        exec_ctx,
                        cmd_name,
                        generator_index,
                        cwd_buf,
                        timeout_ms,
                        transforms,
                        cache,
                        cache_store,
                        js_runtime,
                        env,
                        env_hash,
                    )
                    .await
                }));
                continue;
            }

            if matches!(js_kind, Some(JsRuntimeKind::Custom)) {
                let rt = match gen.js_runtime.as_ref() {
                    Some(rt) => Arc::clone(rt),
                    None => {
                        tracing::warn!(
                            spec = %command,
                            generator_index = idx,
                            kind = ?js_kind,
                            "requires_js generator with ScriptFunction/Custom kind has no js_runtime metadata — skipping"
                        );
                        continue;
                    }
                };
                let exec_ctx = make_js_exec_context(ctx, cwd, shell_env.as_deref());
                let cmd_name = command.to_string();
                let generator_index = idx;
                let js_runtime = Arc::clone(&self.js_runtime);
                let cwd_buf = cwd.to_path_buf();
                let cache = gen.cache.clone();
                let cache_store = Arc::clone(&self.generator_cache);
                let env = shell_env.clone();
                let permit = Arc::clone(&semaphore);
                handles.push(tokio::spawn(async move {
                    let _permit = permit
                        .acquire()
                        .await
                        .map_err(|e| anyhow::anyhow!("semaphore error: {e}"))?;
                    run_custom_dispatch(
                        rt,
                        exec_ctx,
                        cmd_name,
                        generator_index,
                        cwd_buf,
                        timeout_ms,
                        cache,
                        cache_store,
                        js_runtime,
                        env,
                        env_hash,
                    )
                    .await
                }));
                continue;
            }

            if matches!(js_kind, Some(JsRuntimeKind::TokenOnly)) {
                let rt = match gen.js_runtime.as_ref() {
                    Some(rt) => Arc::clone(rt),
                    None => {
                        tracing::warn!(
                            spec = %command,
                            generator_index = idx,
                            kind = ?js_kind,
                            "requires_js generator with TokenOnly kind has no js_runtime metadata — skipping"
                        );
                        continue;
                    }
                };
                let exec_ctx = make_js_exec_context(ctx, cwd, None);
                let cmd_name = command.to_string();
                let generator_index = idx;
                let generator_id = token_only_generator_id(&cmd_name, generator_index);
                if self.token_only_demotion_state.is_demoted(&generator_id) {
                    tracing::warn!(
                        spec = %command,
                        generator_index,
                        generator_id = %generator_id,
                        "token_only generator is demoted after repeated failures — skipping"
                    );
                    continue;
                }
                let js_runtime = Arc::clone(&self.js_runtime);
                let cache = gen.cache.clone();
                let cache_store = Arc::clone(&self.generator_cache);
                let demotion_state = Arc::clone(&self.token_only_demotion_state);
                let permit = Arc::clone(&semaphore);
                handles.push(tokio::spawn(async move {
                    let _permit = permit
                        .acquire()
                        .await
                        .map_err(|e| anyhow::anyhow!("semaphore error: {e}"))?;
                    run_token_only_dispatch(
                        rt,
                        exec_ctx,
                        cmd_name,
                        generator_index,
                        timeout_ms,
                        cache,
                        cache_store,
                        js_runtime,
                        demotion_state,
                        generator_id,
                    )
                    .await
                }));
                continue;
            }

            let cache_cwd = gen
                .cache
                .as_ref()
                .filter(|c| c.cache_by_directory)
                .map(|_| cwd);

            // For non-JS generators, the legacy single-key cache already holds
            // the post-transform suggestion vector — try it first and skip the
            // spawn entirely on a hit. JS-post-process generators can't reuse
            // that path because two different `js_runtime.source` bodies on
            // the same script must NOT share results; we partition them with
            // `CacheKey::JsProcessed { source_hash }` instead.
            if let Some(rt) = js_dispatch.as_ref() {
                // For JS dispatch, peek the post-processed cache up front so a
                // warm hit avoids both the script spawn AND the JS evaluation.
                let js_key = CacheKey::js_processed_with_env(
                    command,
                    &argv,
                    cache_cwd,
                    hash_js_source(&rt.source),
                    env_hash,
                );
                if let Some(cached) = self.generator_cache.get(&js_key) {
                    tracing::debug!("js post-process cache hit for generator {:?}", argv);
                    handles.push(tokio::spawn(async move { Ok::<_, anyhow::Error>(cached) }));
                    continue;
                }
            } else {
                let suggestions_key =
                    CacheKey::from_strings_with_env(command, &argv, cache_cwd, env_hash);
                if let Some(cached) = self.generator_cache.get(&suggestions_key) {
                    tracing::debug!("cache hit for generator {:?}", argv);
                    handles.push(tokio::spawn(async move { Ok::<_, anyhow::Error>(cached) }));
                    continue;
                }
            }

            let permit = Arc::clone(&semaphore);
            let cwd = cwd.to_path_buf();
            let transforms = gen.transforms.clone();
            let cache = gen.cache.clone();
            let cache_store = Arc::clone(&self.generator_cache);
            let cmd_name = command.to_string();
            let js_runtime = Arc::clone(&self.js_runtime);
            let generator_index = idx;
            let env = shell_env.clone();

            handles.push(tokio::spawn(async move {
                let _permit = permit
                    .acquire()
                    .await
                    .map_err(|e| anyhow::anyhow!("semaphore error: {e}"))?;

                let argv_refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();

                // The script runs identically whether or not a JS post-process
                // step follows. Only the cache layer and the post-processing
                // body differ. Stdout cache is keyed by argv only — two
                // generators with the same script share its spawn cost.
                let stdout_key = CacheKey::from_strings_with_env(
                    &cmd_name,
                    &argv,
                    cache_cwd_owned(&cache, &cwd),
                    env_hash,
                );
                let output: String = if let Some(cached) = cache_store.get_stdout(&stdout_key) {
                    tracing::debug!("script stdout cache hit for {:?}", argv);
                    cached
                } else {
                    let fresh =
                        run_script_with_env(&argv_refs, &cwd, timeout_ms, env.as_deref()).await?;
                    if let Some(ref cache_cfg) = cache {
                        if cache_cfg.ttl_seconds > 0 {
                            cache_store.insert_stdout(
                                stdout_key.clone(),
                                fresh.clone(),
                                Duration::from_secs(cache_cfg.ttl_seconds),
                            );
                        }
                    }
                    fresh
                };

                let suggestions = if let Some(rt) = js_dispatch.as_ref() {
                    // JS post-process: feed stdout into the QuickJS evaluator.
                    let timeout = Duration::from_millis(rt.timeout_ms.unwrap_or(timeout_ms));
                    let generator_id = format!("{cmd_name}#{generator_index}");
                    match js_runtime
                        .post_process(&rt.source, output.clone(), timeout, generator_id)
                        .await
                    {
                        Ok(js_output) => match js_output.into_suggestions() {
                            Some(suggs) => suggs
                                .into_iter()
                                .map(|js| Suggestion {
                                    text: js.name,
                                    description: js.description,
                                    kind: SuggestionKind::Command,
                                    source: SuggestionSource::Script,
                                    ..Default::default()
                                })
                                .collect(),
                            // PostProcess jobs always normalise through
                            // `normalize_value` in the worker, which only emits
                            // Suggestions or None — so this arm is the normal
                            // empty/failed path, not a wire-protocol mismatch.
                            // Diagnostics already logged inside the adapter.
                            None => Vec::new(),
                        },
                        Err(e) => {
                            tracing::warn!(
                                spec = %cmd_name,
                                generator_index,
                                error = %e,
                                "js_runtime worker error — returning empty suggestions"
                            );
                            Vec::new()
                        }
                    }
                } else if transforms.is_empty() {
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

                // Cache if configured. The key shape depends on whether we
                // ran a JS post-processor: JS results need their own slot
                // namespaced by source hash, declarative results live under
                // the legacy `Stdout` key shape.
                if let Some(ref cache_cfg) = cache {
                    if cache_cfg.ttl_seconds > 0 && !suggestions.is_empty() {
                        let cache_cwd = if cache_cfg.cache_by_directory {
                            Some(cwd.as_path())
                        } else {
                            None
                        };
                        let key = if let Some(rt) = js_dispatch.as_ref() {
                            CacheKey::js_processed_with_env(
                                &cmd_name,
                                &argv,
                                cache_cwd,
                                hash_js_source(&rt.source),
                                env_hash,
                            )
                        } else {
                            CacheKey::from_strings_with_env(&cmd_name, &argv, cache_cwd, env_hash)
                        };
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
        resolutions: &[ProviderResolution],
        ctx: &ProviderCtx,
        query: &str,
    ) -> Result<Vec<Suggestion>> {
        if resolutions.is_empty() {
            return Ok(Vec::new());
        }
        if !ctx.cwd.is_absolute() {
            tracing::warn!(
                cwd = %ctx.cwd.display(),
                "provider cwd is relative; skipping provider resolution"
            );
            return Ok(Vec::new());
        }
        let mut all = Vec::new();
        for resolution in resolutions {
            let dispatch_ctx = ctx.for_resolution(resolution);
            match providers::resolve(resolution.kind, &dispatch_ctx).await {
                Ok(suggestions) => all.extend(suggestions),
                Err(e) => {
                    tracing::warn!(provider = ?resolution.kind, "provider failed: {e}");
                }
            }
        }
        if query.is_empty() {
            return Ok(all);
        }
        Ok(fuzzy::rank(query, all, MAX_DYNAMIC_CANDIDATES))
    }

    /// Resolve each provider resolution independently and preserve the
    /// per-kind result boundary for callers that need provider-specific
    /// success/error reporting.
    ///
    /// Unlike [`Self::resolve_providers`], this does not aggregate or swallow
    /// per-provider errors. It still applies each [`ProviderResolution`]'s
    /// params map to a cloned [`ProviderCtx`] before dispatching, so callers
    /// do not need to duplicate that overlay logic.
    pub async fn resolve_provider_kinds(
        &self,
        resolutions: &[ProviderResolution],
        ctx: &ProviderCtx,
        query: &str,
    ) -> Vec<(ProviderKind, Result<Vec<Suggestion>>)> {
        let mut out = Vec::with_capacity(resolutions.len());
        for resolution in resolutions {
            let dispatch_ctx = ctx.for_resolution(resolution);
            let res = self
                .resolve_provider_kind(resolution.kind, &dispatch_ctx, query)
                .await;
            out.push((resolution.kind, res));
        }
        out
    }

    /// Per-kind variant of [`Self::resolve_git`] that surfaces errors instead of swallowing.
    pub async fn resolve_git_kind(
        &self,
        kind: git::GitQueryKind,
        cwd: &Path,
        query: &str,
    ) -> Result<Vec<Suggestion>> {
        let suggestions = git::git_suggestions(cwd, kind).await?;
        if query.is_empty() {
            return Ok(suggestions);
        }
        Ok(fuzzy::rank(query, suggestions, MAX_DYNAMIC_CANDIDATES))
    }

    /// Per-kind variant of [`Self::resolve_providers`] that surfaces errors instead of logging-and-swallowing.
    /// The supplied `ctx` is forwarded as-is; callers that need to
    /// apply [`ProviderResolution`] params across multiple independent
    /// dispatches should use [`Self::resolve_provider_kinds`].
    pub async fn resolve_provider_kind(
        &self,
        kind: ProviderKind,
        ctx: &ProviderCtx,
        query: &str,
    ) -> Result<Vec<Suggestion>> {
        if !ctx.cwd.is_absolute() {
            tracing::warn!(
                cwd = %ctx.cwd.display(),
                "provider cwd is relative; skipping provider resolution"
            );
            return Err(anyhow::anyhow!(
                "provider cwd is relative: {}",
                ctx.cwd.display()
            ));
        }
        let suggestions = providers::resolve(kind, ctx).await?;
        if query.is_empty() {
            return Ok(suggestions);
        }
        Ok(fuzzy::rank(query, suggestions, MAX_DYNAMIC_CANDIDATES))
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
        let resolution = specs::resolve_spec(spec.as_ref(), resolve_ctx.as_ref());
        let spec_name = ctx.command.as_deref().unwrap_or("<unknown>");
        let generators = self.filter_script_generators_for_config(
            filter_supported_script_generators(spec_name, resolution.script_generators),
        );
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
        self.suggest_sync_with_env(ctx, cwd, buffer, None)
    }

    pub fn suggest_sync_with_env(
        &self,
        ctx: &CommandContext,
        cwd: &Path,
        buffer: &str,
        shell_env: Option<&HashMap<String, String>>,
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
                self.extend_with_env_vars(ctx, cwd, shell_env, &mut candidates);
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
                self.extend_with_env_vars(ctx, cwd, shell_env, &mut candidates);
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
            let resolution = specs::resolve_spec(spec.as_ref(), resolve_ctx.as_ref());
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
        shell_env: Option<&HashMap<String, String>>,
        candidates: &mut Vec<Suggestion>,
    ) {
        if !ctx.current_word.starts_with('$') {
            return;
        }
        let provided = match shell_env {
            Some(env) => self.env_provider.provide_from_snapshot(ctx, env),
            None => self.env_provider.provide(ctx, cwd),
        };
        match provided {
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
    fn spec_for_ctx(&self, ctx: &CommandContext) -> Option<Arc<specs::CompletionSpec>> {
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
            mut provider_generators,
            mut script_generators,
            wants_filepaths,
            wants_folders_only,
            preceding_flag_has_args,
            past_double_dash,
            static_suggestions,
        } = specs::resolve_spec(spec.as_ref(), resolve_ctx.as_ref());

        if resolve_ctx.command.as_deref() == Some("aws") {
            let globals = aws_cli_globals(resolve_ctx.as_ref());
            provider_generators =
                apply_aws_cli_globals_to_provider_generators(provider_generators, &globals);
            script_generators =
                apply_aws_cli_globals_to_script_generators(script_generators, &globals);
        }

        let git_generators = self.git_generators_from(&native_generators);

        // Suppress subcommands/options when:
        // 1. The preceding flag takes an argument (e.g. `curl -o <TAB>`)
        // 2. We're past `--` (end-of-flags separator) — only positional args
        let suppress_commands = preceding_flag_has_args || past_double_dash;

        if !suppress_commands {
            candidates.extend(subcommands);
            candidates.extend(options);
        }

        // Static `args.suggestions` are values for an arg position, not commands —
        // they MUST surface even when `suppress_commands` is true (preceding flag
        // has args, or past `--`). Keep this extend OUTSIDE the suppression guard.
        candidates.extend(static_suggestions);

        if wants_folders_only && self.providers_filesystem {
            self.extend_with_folders(ctx, cwd, &mut candidates);
        } else if wants_filepaths && self.providers_filesystem {
            match self.filesystem_provider.provide(ctx, cwd) {
                Ok(fs) => candidates.extend(fs),
                Err(e) => tracing::warn!("filesystem provider error: {e}"),
            }
        }

        // Script generators are dispatched asynchronously by the caller.
        let spec_name = ctx.command.as_deref().unwrap_or("<unknown>");
        let script_generators = self.filter_script_generators_for_config(
            filter_supported_script_generators(spec_name, script_generators),
        );
        let provider_generators = self.filter_provider_generators_for_config(provider_generators);

        let suggestions = self.rank_with_history(ctx, cwd, buffer, candidates, true);

        Ok(SyncResult {
            suggestions,
            script_generators,
            git_generators,
            provider_generators,
        })
    }

    fn filter_provider_generators_for_config(
        &self,
        generators: Vec<ProviderResolution>,
    ) -> Vec<ProviderResolution> {
        generators
            .into_iter()
            .filter(|resolution| resolution.kind != ProviderKind::AwsSdk || self.providers_aws_sdk)
            .collect()
    }

    fn filter_script_generators_for_config(
        &self,
        generators: Vec<Arc<GeneratorSpec>>,
    ) -> Vec<Arc<GeneratorSpec>> {
        generators
            .into_iter()
            .filter(|gen| {
                if !is_aws_sdk_fallback_generator(gen) {
                    return true;
                }
                // `aws_sdk_fallback_to_cli = false` only suppresses the CLI
                // fallback when the native AWS provider is *also* enabled —
                // the flag means "the native provider supersedes CLI". If
                // the native provider is off, CLI is the only path that
                // produces completions, so an explicit `fallback = false`
                // must not strand the user with an empty popup.
                !self.providers_aws_sdk || self.aws_sdk_fallback_to_cli
            })
            .collect()
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
        // Cap of high-confidence history rows reserved before normal
        // candidates are ranked. Two is enough to keep the most recent
        // exact/prefix match visible without crowding flags or refs.
        const RESERVED_HISTORY: usize = 2;

        // Flag context (current_word starts with '-') and redirect context
        // both want a different lane than command-history: flags don't
        // prefix-match command lines, and redirects expect filenames. The
        // `!ctx.is_flag` clause deliberately suppresses history entirely in
        // flag context (e.g. buffer `git --`): a prefix-matching command
        // line such as `git --version` is noise next to spec flags, so it is
        // dropped on purpose rather than fuzzy-ranked into the popup.
        let history_lane_allowed =
            include_history && self.max_history_results > 0 && !ctx.in_redirect && !ctx.is_flag;

        // Fetch history once when the lane is allowed. The same Vec feeds
        // both the reservation count and the existing fuzzy-fill step
        // below, so we avoid a second `history_provider.provide` round.
        let history_entries: Vec<Suggestion> = if history_lane_allowed {
            match self.history_provider.provide(ctx, cwd) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!("history provider error: {e}");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        // Reserve up to RESERVED_HISTORY exact/prefix-matching rows. The
        // budget MUST be reduced BEFORE `fuzzy::rank` runs — capping only
        // after the rank lets a saturated candidate set grow the popup
        // past `max_results` once history is appended.
        // Clamp the reservation to what history can actually fill below
        // (`max_history_results`). Otherwise, with `max_history_results = 1`
        // and two prefix-matching entries, we would shrink `normal_budget`
        // by 2 but only append 1 history row, wasting a popup slot.
        let reserved_history = history_entries
            .iter()
            .filter(|s| s.text == buffer || s.text.starts_with(buffer))
            .take(RESERVED_HISTORY)
            .count()
            .min(self.max_history_results);
        let normal_budget = self.max_results.saturating_sub(reserved_history);

        let mut results = fuzzy::rank(&ctx.current_word, candidates, normal_budget);

        // History fuzzy-fill: with `normal_budget` shrunk above, there is
        // now at least `reserved_history` extra room for the entries that
        // drove the reservation. Additional low-confidence matches fill
        // any further slack up to `max_history_results`.
        if !history_entries.is_empty() {
            let remaining = self
                .max_history_results
                .min(self.max_results.saturating_sub(results.len()));
            if remaining > 0 {
                results.extend(fuzzy::rank(buffer, history_entries, remaining));
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

/// Helper: resolve the cwd argument used by the cache key based on the
/// generator's [`crate::specs::CacheConfig::cache_by_directory`] flag. Lives
/// here rather than in `cache.rs` so the engine task body can stay
/// borrow-checker friendly when a cwd reference is needed inside an `async
/// move` block.
fn cache_cwd_owned<'a>(
    cache: &'a Option<crate::specs::CacheConfig>,
    cwd: &'a Path,
) -> Option<&'a Path> {
    cache.as_ref().filter(|c| c.cache_by_directory).map(|_| cwd)
}

fn token_only_generator_id(cmd_name: &str, generator_index: usize) -> String {
    format!("{cmd_name}#{generator_index}#token_only")
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

fn is_supported_script_generator(gen: &GeneratorSpec) -> bool {
    if !gen.requires_js {
        return true;
    }

    match gen.js_runtime.as_ref() {
        // Mirror doctor::count_missing_js_runtime_in_spec / validate.rs
        // emptiness check so a hand-written user spec with empty source
        // can't slip past the engine while the doctor flags it as
        // missing — the engine would otherwise build a JS program that
        // surfaces a SyntaxError diagnostic on every keystroke.
        Some(rt) if rt.kind == JsRuntimeKind::PostProcess => {
            has_non_empty_script_or_template(gen) && !rt.source.trim().is_empty()
        }
        Some(rt)
            if matches!(
                rt.kind,
                JsRuntimeKind::ScriptFunction | JsRuntimeKind::Custom
            ) =>
        {
            rt.self_contained && !rt.source.trim().is_empty()
        }
        Some(rt) if rt.kind == JsRuntimeKind::TokenOnly => !rt.source.trim().is_empty(),
        _ => false,
    }
}

fn has_non_empty_script_or_template(gen: &GeneratorSpec) -> bool {
    gen.script.as_ref().is_some_and(|script| !script.is_empty())
        || gen
            .script_template
            .as_ref()
            .is_some_and(|template| !template.is_empty())
}

fn is_aws_sdk_fallback_generator(gen: &GeneratorSpec) -> bool {
    gen.generator_type.as_deref() == Some(ProviderKind::AwsSdk.type_str())
        && has_non_empty_script_or_template(gen)
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct AwsCliGlobals {
    profile: Option<String>,
    region: Option<String>,
}

impl AwsCliGlobals {
    fn is_empty(&self) -> bool {
        self.profile.is_none() && self.region.is_none()
    }
}

fn aws_cli_globals(ctx: &CommandContext) -> AwsCliGlobals {
    let mut globals = AwsCliGlobals::default();
    let args = &ctx.args;
    let mut idx = 0;

    while idx < args.len() {
        let arg = args[idx].as_str();
        if let Some(value) = arg.strip_prefix("--profile=") {
            if let Some(value) = clean_cli_global_value(value) {
                globals.profile = Some(value.to_string());
            }
            idx += 1;
            continue;
        }
        if arg == "--profile" {
            if let Some(value) = args
                .get(idx + 1)
                .and_then(|value| clean_cli_global_value(value))
            {
                globals.profile = Some(value.to_string());
                idx += 2;
            } else {
                idx += 1;
            }
            continue;
        }
        if let Some(value) = arg.strip_prefix("--region=") {
            if let Some(value) = clean_cli_global_value(value) {
                globals.region = Some(value.to_string());
            }
            idx += 1;
            continue;
        }
        if arg == "--region" {
            if let Some(value) = args
                .get(idx + 1)
                .and_then(|value| clean_cli_global_value(value))
            {
                globals.region = Some(value.to_string());
                idx += 2;
            } else {
                idx += 1;
            }
            continue;
        }
        idx += 1;
    }

    globals
}

fn clean_cli_global_value(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn apply_aws_cli_globals_to_provider_generators(
    generators: Vec<ProviderResolution>,
    globals: &AwsCliGlobals,
) -> Vec<ProviderResolution> {
    if globals.is_empty() {
        return generators;
    }

    generators
        .into_iter()
        .map(|mut resolution| {
            if resolution.kind == ProviderKind::AwsSdk {
                let mut params: BTreeMap<String, String> = resolution.params.as_ref().clone();
                if let Some(profile) = &globals.profile {
                    params.insert("profile".to_string(), profile.clone());
                }
                if let Some(region) = &globals.region {
                    params.insert("region".to_string(), region.clone());
                }
                resolution.params = Arc::new(params);
            }
            resolution
        })
        .collect()
}

fn apply_aws_cli_globals_to_script_generators(
    generators: Vec<Arc<GeneratorSpec>>,
    globals: &AwsCliGlobals,
) -> Vec<Arc<GeneratorSpec>> {
    if globals.is_empty() {
        return generators;
    }

    generators
        .into_iter()
        .map(|gen| {
            if !is_aws_sdk_fallback_generator(&gen) {
                return gen;
            }
            let Some(script) = gen.script.as_ref() else {
                return gen;
            };
            let rewritten = aws_script_with_globals(script, globals);
            if rewritten == *script {
                return gen;
            }

            let mut clone = gen.as_ref().clone();
            clone.script = Some(rewritten);
            Arc::new(clone)
        })
        .collect()
}

fn aws_script_with_globals(script: &[String], globals: &AwsCliGlobals) -> Vec<String> {
    if globals.is_empty() || script.first().map(String::as_str) != Some("aws") {
        return script.to_vec();
    }

    let mut rewritten = Vec::with_capacity(script.len() + 4);
    rewritten.push("aws".to_string());
    if let Some(profile) = &globals.profile {
        rewritten.push("--profile".to_string());
        rewritten.push(profile.clone());
    }
    if let Some(region) = &globals.region {
        rewritten.push("--region".to_string());
        rewritten.push(region.clone());
    }

    let mut idx = 1;
    while idx < script.len() {
        let arg = script[idx].as_str();
        if matches!(arg, "--profile" | "--region") {
            idx += 2;
            continue;
        }
        if arg.starts_with("--profile=") || arg.starts_with("--region=") {
            idx += 1;
            continue;
        }
        rewritten.push(script[idx].clone());
        idx += 1;
    }

    rewritten
}

/// Filter a generator slice through [`is_supported_script_generator`]
/// while emitting a `tracing::trace!` for each rejected generator.
///
/// The filter itself is on the keystroke hot path, so the log level is
/// deliberately `trace` (off by default) — operators chasing "this
/// completion stopped working after I edited my user spec" can opt in
/// via `RUST_LOG=gc_suggest=trace` and see the spec name + generator
/// index + a coarse reason without the spec author having to re-run
/// `ghost-complete doctor`. The doctor remains the actionable surface;
/// this trace is purely a correlation aid.
fn filter_supported_script_generators(
    spec_name: &str,
    generators: impl IntoIterator<Item = Arc<GeneratorSpec>>,
) -> Vec<Arc<GeneratorSpec>> {
    generators
        .into_iter()
        .enumerate()
        .filter_map(|(idx, g)| {
            if is_supported_script_generator(&g) {
                Some(g)
            } else {
                tracing::trace!(
                    spec = %spec_name,
                    generator_index = idx,
                    requires_js = g.requires_js,
                    kind = ?g.js_runtime.as_ref().map(|rt| &rt.kind),
                    has_script = g.script.is_some(),
                    has_script_template = g.script_template.is_some(),
                    "engine: dropping requires_js generator that fails dispatch predicate \
                     (see ghost-complete doctor for the actionable surface)"
                );
                None
            }
        })
        .collect()
}

/// Pack the parsed command line into the host-API context for a JS
/// dispatch. The token slice is `[command, ...completed_args,
/// current_word]`, including an empty final slot at a word boundary, so
/// Fig-style code that reads `tokens[tokens.length - 1]` sees the live
/// cursor token.
///
/// Mirrors the full process environment except `GHOST_COMPLETE_ACTIVE`
/// (matching `script::run_script`).
fn make_js_exec_context(
    ctx: &CommandContext,
    cwd: &Path,
    shell_env: Option<&HashMap<String, String>>,
) -> JsExecContext {
    let mut tokens: Vec<String> = Vec::with_capacity(2 + ctx.args.len());
    if let Some(cmd) = ctx.command.as_ref() {
        tokens.push(cmd.clone());
    }
    tokens.extend(ctx.args.iter().cloned());
    tokens.push(ctx.current_word.clone());

    // Snapshot the live shell env when available, otherwise fall back to the
    // proxy process env so generators that read `env.HOME` / `env.PATH` keep
    // working. Mutations inside JS are confined to the host object and do not
    // leak back into the engine.
    let env_iter: Box<dyn Iterator<Item = (String, String)> + '_> = match shell_env {
        Some(env) => Box::new(env.iter().map(|(k, v)| (k.clone(), v.clone()))),
        None => Box::new(std::env::vars()),
    };
    let mut env = std::collections::BTreeMap::new();
    for (k, v) in env_iter {
        if k == "GHOST_COMPLETE_ACTIVE" {
            continue;
        }
        env.insert(k, v);
    }

    JsExecContext {
        tokens,
        current_token: ctx.current_word.clone(),
        previous_token: ctx
            .args
            .last()
            .cloned()
            .unwrap_or_else(|| ctx.command.clone().unwrap_or_default()),
        cwd: cwd.to_path_buf(),
        env,
    }
}

/// Run a `script_function` generator. JS produces argv; the engine then
/// runs the argv through `run_script` with the generator's transform
/// pipeline. Returns the resulting suggestions (post-transform,
/// post-cache).
#[allow(clippy::too_many_arguments)]
async fn run_script_function_dispatch(
    rt: Arc<specs::JsRuntimeSpec>,
    exec_ctx: JsExecContext,
    cmd_name: String,
    generator_index: usize,
    cwd: PathBuf,
    timeout_ms: u64,
    transforms: Vec<crate::transform::Transform>,
    cache: Option<crate::specs::CacheConfig>,
    cache_store: Arc<crate::cache::GeneratorCache>,
    js_runtime: Arc<JsRuntimeAdapter>,
    env: Option<Arc<HashMap<String, String>>>,
    env_hash: Option<u64>,
) -> Result<Vec<Suggestion>> {
    let timeout = Duration::from_millis(rt.timeout_ms.unwrap_or(timeout_ms));
    let generator_id = format!("{cmd_name}#{generator_index}#script_function");

    // Resolve argv via JS first.
    let js_output = match js_runtime
        .script_function(&rt.source, exec_ctx, timeout, generator_id)
        .await
    {
        Ok(out) => out,
        Err(e) => {
            tracing::warn!(
                spec = %cmd_name,
                generator_index,
                error = %e,
                "js_runtime worker error during script_function — returning empty suggestions"
            );
            return Ok(Vec::new());
        }
    };
    // ScriptFunction jobs always normalise through `normalize_argv` in
    // the worker (see worker.rs `run_job`), which only emits Argv or
    // None — so the wildcard arms below are defensive but unreachable.
    // Diagnostics from the runtime are already logged by the adapter on
    // the way out.
    let argv: Vec<String> = match js_output.into_argv() {
        Some(v) if !v.is_empty() => v,
        Some(_) => return Ok(Vec::new()),
        None => return Ok(Vec::new()),
    };

    // Run the resolved argv. We honour the same caching scheme as a
    // declarative script generator — both stdout and the
    // post-processed suggestion vec are keyed by the resolved argv,
    // namespaced by `kind=script_function` so a future post-process
    // generator with the same script doesn't share the cache slot.
    let cache_cwd = cache_cwd_owned(&cache, &cwd);
    let suggestions_key = CacheKey::js_processed_with_env(
        &cmd_name,
        &argv,
        cache_cwd,
        hash_js_source(&format!("script_function:{}", rt.source)),
        env_hash,
    );
    if let Some(cached) = cache_store.get(&suggestions_key) {
        tracing::debug!("script_function cache hit for {:?}", argv);
        return Ok(cached);
    }
    let stdout_key =
        CacheKey::from_strings_with_env(&cmd_name, &argv, cache_cwd_owned(&cache, &cwd), env_hash);
    let argv_refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    let stdout: String = if let Some(cached) = cache_store.get_stdout(&stdout_key) {
        tracing::debug!("script_function stdout cache hit for {:?}", argv);
        cached
    } else {
        let fresh = run_script_with_env(&argv_refs, &cwd, timeout_ms, env.as_deref()).await?;
        if let Some(ref cache_cfg) = cache {
            if cache_cfg.ttl_seconds > 0 {
                cache_store.insert_stdout(
                    stdout_key.clone(),
                    fresh.clone(),
                    Duration::from_secs(cache_cfg.ttl_seconds),
                );
            }
        }
        fresh
    };

    let suggestions: Vec<Suggestion> = if transforms.is_empty() {
        stdout
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
        execute_pipeline(&stdout, &transforms).map_err(|e| anyhow::anyhow!("{e}"))?
    };

    if let Some(ref cache_cfg) = cache {
        if cache_cfg.ttl_seconds > 0 && !suggestions.is_empty() {
            cache_store.insert(
                suggestions_key,
                suggestions.clone(),
                Duration::from_secs(cache_cfg.ttl_seconds),
            );
        }
    }
    Ok(suggestions)
}

/// Run a `custom` generator. JS evaluates with the host
/// `executeShellCommand` binding and returns suggestions directly. The
/// engine never touches the script path — the runner trait does that
/// on JS's behalf.
#[allow(clippy::too_many_arguments)]
async fn run_custom_dispatch(
    rt: Arc<specs::JsRuntimeSpec>,
    exec_ctx: JsExecContext,
    cmd_name: String,
    generator_index: usize,
    cwd: PathBuf,
    timeout_ms: u64,
    cache: Option<crate::specs::CacheConfig>,
    cache_store: Arc<crate::cache::GeneratorCache>,
    js_runtime: Arc<JsRuntimeAdapter>,
    env: Option<Arc<HashMap<String, String>>>,
    env_hash: Option<u64>,
) -> Result<Vec<Suggestion>> {
    let timeout = Duration::from_millis(rt.timeout_ms.unwrap_or(timeout_ms));
    let generator_id = format!("{cmd_name}#{generator_index}#custom");

    // Cache key: namespaced by `kind=custom` plus the source hash. The
    // tokens contribute a coarse fingerprint so that two distinct
    // typed prefixes don't collide on the same custom generator. The
    // engine's existing cache shape doesn't have a "tokens" axis, so
    // we fold the tokens into the source-hash slot to keep the change
    // local.
    //
    // cwd is unconditionally included even when `cache_by_directory` is
    // false: a Custom generator typically reads cwd through the JS host
    // API, so two invocations in different dirs would otherwise share a
    // cache slot (`git checkout` in repo A would surface repo B's
    // branches).
    let token_fingerprint = exec_ctx.tokens.join("\u{1}");
    let key_source = format!(
        "custom:{src}#tokens:{tokens}#current:{current}#previous:{previous}",
        src = rt.source,
        tokens = token_fingerprint,
        current = exec_ctx.current_token,
        previous = exec_ctx.previous_token,
    );
    let cache_key = CacheKey::js_processed_with_env(
        &cmd_name,
        std::slice::from_ref(&cmd_name), // argv slot is unused for custom
        Some(cwd.as_path()),
        hash_js_source(&key_source),
        env_hash,
    );
    if let Some(cached) = cache_store.get(&cache_key) {
        tracing::debug!("custom cache hit for spec {}", cmd_name);
        return Ok(cached);
    }

    // Build a runner backed by the current tokio runtime so the worker
    // can `block_on` synchronously inside `executeShellCommand`.
    let runner = EngineShellRunner::from_current_handle_with_env(env).into_arc();

    let js_output = match js_runtime
        .custom(
            &rt.source,
            exec_ctx,
            timeout,
            generator_id,
            runner,
            rt.allow_shell_command,
        )
        .await
    {
        Ok(out) => out,
        Err(e) => {
            tracing::warn!(
                spec = %cmd_name,
                generator_index,
                error = %e,
                "js_runtime worker error during custom — returning empty suggestions"
            );
            return Ok(Vec::new());
        }
    };
    // Custom jobs always normalise through `normalize_value` in the
    // worker (see worker.rs `run_job`), which only emits Suggestions or
    // None — so the None arm here is the normal "empty / failed" path,
    // not a wire-protocol mismatch. Diagnostics from the runtime are
    // already logged by the adapter on the way out.
    let suggestions: Vec<Suggestion> = match js_output.into_suggestions() {
        Some(suggs) => suggs
            .into_iter()
            .map(|js| Suggestion {
                text: js.name,
                description: js.description,
                kind: SuggestionKind::Command,
                source: SuggestionSource::Script,
                ..Default::default()
            })
            .collect(),
        None => Vec::new(),
    };

    if let Some(ref cache_cfg) = cache {
        if cache_cfg.ttl_seconds > 0 && !suggestions.is_empty() {
            cache_store.insert(
                cache_key,
                suggestions.clone(),
                Duration::from_secs(cache_cfg.ttl_seconds),
            );
        }
    }
    Ok(suggestions)
}

/// Run a `token_only` generator. JS evaluates with only token globals
/// installed by gc-jsrt; the engine does not expose cwd/env/fig or run a
/// subprocess. The result normalises through the same suggestion shapes as
/// `custom`.
#[allow(clippy::too_many_arguments)]
async fn run_token_only_dispatch(
    rt: Arc<specs::JsRuntimeSpec>,
    exec_ctx: JsExecContext,
    cmd_name: String,
    generator_index: usize,
    timeout_ms: u64,
    cache: Option<crate::specs::CacheConfig>,
    cache_store: Arc<crate::cache::GeneratorCache>,
    js_runtime: Arc<JsRuntimeAdapter>,
    demotion_state: Arc<TokenOnlyDemotionState>,
    generator_id: String,
) -> Result<Vec<Suggestion>> {
    let timeout = Duration::from_millis(rt.timeout_ms.unwrap_or(timeout_ms));

    let token_fingerprint = exec_ctx.tokens.join("\u{1}");
    let key_source = format!(
        "token_only:{src}#tokens:{tokens}#current:{current}#previous:{previous}",
        src = rt.source,
        tokens = token_fingerprint,
        current = exec_ctx.current_token,
        previous = exec_ctx.previous_token,
    );
    let cache_key = CacheKey::js_processed(
        &cmd_name,
        std::slice::from_ref(&cmd_name),
        None,
        hash_js_source(&key_source),
    );
    if let Some(cached) = cache_store.get(&cache_key) {
        tracing::debug!("token_only cache hit for spec {}", cmd_name);
        return Ok(cached);
    }

    let js_output = match js_runtime
        .token_only(&rt.source, exec_ctx, timeout, generator_id.clone())
        .await
    {
        Ok(out) => out,
        Err(e) => {
            tracing::warn!(
                spec = %cmd_name,
                generator_index,
                error = %e,
                "js_runtime worker error during token_only — returning empty suggestions"
            );
            return Ok(Vec::new());
        }
    };
    let had_timeout = js_output
        .diagnostics
        .iter()
        .any(|diag| diag.code == JsDiagnosticCode::Timeout);
    let had_hard_failure = js_output.diagnostics.iter().any(|diag| {
        matches!(
            diag.code,
            JsDiagnosticCode::Exception
                | JsDiagnosticCode::MemoryExceeded
                | JsDiagnosticCode::OversizedOutput
        )
    });
    let has_real_success = matches!(
        &js_output.payload,
        JsRuntimeOutputPayload::Suggestions(v) if !v.is_empty()
    );

    if had_timeout {
        let consecutive = demotion_state.record_timeout(&generator_id);
        if consecutive >= TOKEN_ONLY_DEMOTE_AFTER_FAILURES {
            tracing::warn!(
                spec = %cmd_name,
                generator_index,
                generator_id = %generator_id,
                consecutive_failures = consecutive,
                "token_only generator demoted after repeated timeouts"
            );
        }
    } else if had_hard_failure {
        let consecutive = demotion_state.record_failure(&generator_id);
        if consecutive >= TOKEN_ONLY_DEMOTE_AFTER_FAILURES {
            tracing::warn!(
                spec = %cmd_name,
                generator_index,
                generator_id = %generator_id,
                consecutive_failures = consecutive,
                "token_only generator demoted after repeated runtime failures"
            );
        }
    } else if has_real_success {
        demotion_state.record_success(&generator_id);
    }
    let suggestions: Vec<Suggestion> = match js_output.into_suggestions() {
        Some(suggs) => suggs
            .into_iter()
            .map(|js| Suggestion {
                text: js.name,
                description: js.description,
                kind: SuggestionKind::Command,
                source: SuggestionSource::Script,
                ..Default::default()
            })
            .collect(),
        None => Vec::new(),
    };

    if let Some(ref cache_cfg) = cache {
        if cache_cfg.ttl_seconds > 0 && !suggestions.is_empty() {
            cache_store.insert(
                cache_key,
                suggestions.clone(),
                Duration::from_secs(cache_cfg.ttl_seconds),
            );
        }
    }
    Ok(suggestions)
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
    fn direct_non_empty_spec_dirs_do_not_register_embedded_specs() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("only-custom.json"),
            r#"{"name":"only-custom","subcommands":[{"name":"local-only"}]}"#,
        )
        .unwrap();

        let engine = SuggestionEngine::new(&[dir.path().to_path_buf()]).unwrap();

        assert!(engine.spec_store.get("only-custom").is_some());
        assert!(
            engine.spec_store.get("git").is_none(),
            "direct non-empty spec dirs must not be supplemented by embedded-only commands"
        );
    }

    #[test]
    fn explicit_embedded_constructor_can_supplement_non_empty_dirs() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("only-custom.json"),
            r#"{"name":"only-custom","subcommands":[{"name":"local-only"}]}"#,
        )
        .unwrap();

        let engine =
            SuggestionEngine::new_with_embedded(&[dir.path().to_path_buf()], true).unwrap();

        assert!(engine.spec_store.get("only-custom").is_some());
        assert!(
            engine.spec_store.get("git").is_some(),
            "explicit embedded policy should supplement runtime dirs with embedded specs"
        );
    }

    #[test]
    fn constructor_registers_embedded_specs_without_parsing_them() {
        let engine = SuggestionEngine::new(&[]).unwrap();

        assert!(
            !engine.spec_store.is_empty(),
            "embedded corpus must be registered by the daemon constructor"
        );
        let parsed_count = engine
            .spec_store
            .entries()
            .iter()
            .filter(|entry| entry.is_parsed())
            .count();
        assert_eq!(
            parsed_count, 0,
            "daemon constructor must not force-parse embedded specs"
        );
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
            .with_suggest_config(50, false, 5, true, true, true, true);

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

    fn aws_sdk_routing_engine() -> (tempfile::TempDir, SuggestionEngine) {
        let spec_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            spec_dir.path().join("awsx.json"),
            r#"{
                "name": "awsx",
                "args": [{
                    "generators": [{
                        "type": "aws_sdk",
                        "params": {
                            "service": "iam",
                            "operation": "ListRoles",
                            "field": "Roles[*].RoleName",
                            "description_field": "Roles[*].Arn"
                        },
                        "script": ["printf", "{\"Roles\":[{\"RoleName\":\"fallback\"}]}"],
                        "transforms": [{
                            "type": "json_path_extract",
                            "array": "Roles",
                            "name_field": "RoleName"
                        }]
                    }]
                }]
            }"#,
        )
        .unwrap();
        let spec_store = SpecStore::load_from_dir(spec_dir.path()).unwrap().store;
        let history = HistoryProvider::from_entries(vec![]);
        let commands = CommandsProvider::from_list(vec![]);
        (
            spec_dir,
            SuggestionEngine::with_providers(spec_store, history, commands),
        )
    }

    #[test]
    fn aws_sdk_provider_is_default_off_with_cli_fallback_kept() {
        let (_spec_dir, engine) = aws_sdk_routing_engine();
        let ctx = make_ctx(Some("awsx"), vec![], "", 1);

        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "awsx ")
            .unwrap();

        assert!(results.provider_generators.is_empty());
        assert_eq!(results.script_generators.len(), 1);
    }

    #[test]
    fn aws_sdk_provider_enabled_routes_provider_and_respects_fallback_flag() {
        let ctx = make_ctx(Some("awsx"), vec![], "", 1);

        let (_spec_dir_a, with_fallback) = aws_sdk_routing_engine();
        let with_fallback = with_fallback.with_aws_sdk_config(true, true);
        let results = with_fallback
            .suggest_sync(&ctx, Path::new("/tmp"), "awsx ")
            .unwrap();
        assert_eq!(results.provider_generators.len(), 1);
        assert_eq!(results.provider_generators[0].kind, ProviderKind::AwsSdk);
        assert_eq!(results.script_generators.len(), 1);

        let (_spec_dir_b, without_fallback) = aws_sdk_routing_engine();
        let without_fallback = without_fallback.with_aws_sdk_config(true, false);
        let results = without_fallback
            .suggest_sync(&ctx, Path::new("/tmp"), "awsx ")
            .unwrap();
        assert_eq!(results.provider_generators.len(), 1);
        assert!(results.script_generators.is_empty());
    }

    #[test]
    fn aws_sdk_fallback_flag_only_applies_when_native_provider_enabled() {
        // `aws_sdk_fallback_to_cli = false` is meant for "the native
        // provider supersedes CLI"; when the native provider is OFF the
        // flag must NOT also suppress the CLI script, otherwise the
        // popup stays empty for a user who never opted into the native
        // path.
        let (_spec_dir, engine) = aws_sdk_routing_engine();
        let engine = engine.with_aws_sdk_config(false, false);
        let ctx = make_ctx(Some("awsx"), vec![], "", 1);

        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "awsx ")
            .unwrap();

        assert!(results.provider_generators.is_empty());
        assert_eq!(
            results.script_generators.len(),
            1,
            "CLI fallback must survive an explicit fallback=false when the native provider is also off"
        );
    }

    #[test]
    fn aws_sdk_provider_params_include_typed_profile() {
        let engine = make_engine().with_aws_sdk_config(true, true);
        let buffer = "aws --profile loftyworks-pay-dev iam attach-role-policy --role-name ";
        let ctx = gc_buffer::parse_command_context(buffer, buffer.chars().count());

        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), buffer)
            .unwrap();
        let provider = results
            .provider_generators
            .iter()
            .find(|resolution| resolution.kind == ProviderKind::AwsSdk)
            .expect("aws_sdk provider should be scheduled for --role-name");

        assert_eq!(
            provider.params.get("profile").map(String::as_str),
            Some("loftyworks-pay-dev"),
            "typed --profile must override inherited AWS_PROFILE for provider dispatch"
        );
    }

    #[test]
    fn aws_cli_fallback_script_includes_typed_profile() {
        let engine = make_engine();
        let buffer = "aws --profile loftyworks-pay-dev iam attach-role-policy --role-name ";
        let ctx = gc_buffer::parse_command_context(buffer, buffer.chars().count());

        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), buffer)
            .unwrap();
        let script = results
            .script_generators
            .iter()
            .find(|gen| gen.generator_type.as_deref() == Some("aws_sdk"))
            .and_then(|gen| gen.script.as_ref())
            .expect("aws_sdk CLI fallback script should be scheduled");

        assert_eq!(
            script
                .iter()
                .take(5)
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![
                "aws",
                "--profile",
                "loftyworks-pay-dev",
                "iam",
                "list-roles"
            ],
            "fallback CLI script must pass the typed profile before the service command"
        );
    }

    #[test]
    fn suggest_sync_with_env_uses_shell_reported_env_vars() {
        let engine = make_engine();
        let ctx = make_ctx(Some("echo"), vec![], "$AWS", 1);
        let env = HashMap::from([("AWS_PROFILE".to_string(), "session".to_string())]);

        let results = engine
            .suggest_sync_with_env(&ctx, Path::new("/tmp"), "echo $AWS", Some(&env))
            .unwrap();

        assert!(
            results.suggestions.iter().any(|s| s.text == "$AWS_PROFILE"),
            "shell-reported env should drive $VAR suggestions, got {:?}",
            results
                .suggestions
                .iter()
                .map(|s| &s.text)
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn run_generators_with_env_uses_env_and_partitions_cache_by_env() {
        let engine = make_engine();
        let ctx = make_ctx(Some("envtest"), vec![], "", 1);
        let gen: specs::GeneratorSpec = serde_json::from_value(serde_json::json!({
            "script": ["/bin/sh", "-c", "printf '%s\\n' \"$AWS_PROFILE\""],
            "cache": { "ttl_seconds": 60 }
        }))
        .unwrap();
        let generators = vec![Arc::new(gen)];
        let cwd = Path::new("/tmp");

        let first_env = Arc::new(HashMap::from([(
            "AWS_PROFILE".to_string(),
            "dev-profile".to_string(),
        )]));
        let first = engine
            .run_generators_with_env(&generators, &ctx, cwd, 5_000, Some(first_env))
            .await
            .unwrap();

        let second_env = Arc::new(HashMap::from([(
            "AWS_PROFILE".to_string(),
            "prod-profile".to_string(),
        )]));
        let second = engine
            .run_generators_with_env(&generators, &ctx, cwd, 5_000, Some(second_env))
            .await
            .unwrap();

        assert_eq!(first[0].text, "dev-profile");
        assert_eq!(
            second[0].text, "prod-profile",
            "changing shell env must not reuse cached script output from the prior profile"
        );
    }

    #[test]
    fn aws_cli_globals_support_inline_profile_and_late_region() {
        let engine = make_engine().with_aws_sdk_config(true, true);
        let buffer = "aws iam attach-role-policy --profile=pay-dev --region eu-west-1 --role-name ";
        let ctx = gc_buffer::parse_command_context(buffer, buffer.chars().count());

        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), buffer)
            .unwrap();
        let provider = results
            .provider_generators
            .iter()
            .find(|resolution| resolution.kind == ProviderKind::AwsSdk)
            .expect("aws_sdk provider should be scheduled for --role-name");
        assert_eq!(
            provider.params.get("profile").map(String::as_str),
            Some("pay-dev")
        );
        assert_eq!(
            provider.params.get("region").map(String::as_str),
            Some("eu-west-1")
        );

        let script = results
            .script_generators
            .iter()
            .find(|gen| gen.generator_type.as_deref() == Some("aws_sdk"))
            .and_then(|gen| gen.script.as_ref())
            .expect("aws_sdk CLI fallback script should be scheduled");
        assert_eq!(
            script
                .iter()
                .take(7)
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![
                "aws",
                "--profile",
                "pay-dev",
                "--region",
                "eu-west-1",
                "iam",
                "list-roles",
            ]
        );
    }

    #[tokio::test]
    async fn test_resolve_providers_relative_cwd_returns_empty() {
        // A relative cwd would make local-project provider ancestor walks
        // consult the ghost-complete process cwd. The provider dispatch
        // boundary must reject it before any provider can read manifests
        // from the wrong project.
        assert!(
            Path::new(".").join("Cargo.toml").is_file(),
            "test requires the process cwd to contain a Cargo.toml"
        );
        let engine = make_engine();
        let ctx = crate::providers::ProviderCtx::new_for_test(
            PathBuf::from("."),
            Arc::new(std::collections::HashMap::new()),
            String::new(),
        );

        let results = engine
            .resolve_providers(&[ProviderKind::CargoWorkspaceMembers.into()], &ctx, "")
            .await
            .unwrap();

        assert!(
            results.is_empty(),
            "relative cwd must not resolve providers from process cwd, got {results:?}"
        );
    }

    #[tokio::test]
    async fn test_resolve_provider_kind_relative_cwd_returns_err() {
        let engine = make_engine();
        let ctx = crate::providers::ProviderCtx::new_for_test(
            PathBuf::from("relative/path"),
            Arc::new(std::collections::HashMap::new()),
            String::new(),
        );

        let result = engine
            .resolve_provider_kind(ProviderKind::CargoWorkspaceMembers, &ctx, "")
            .await;

        let err = result.expect_err("relative cwd must surface an error to the caller");
        let msg = err.to_string();
        assert!(
            msg.contains("relative") && msg.contains("relative/path"),
            "error must name both the cause and the offending path, got {msg:?}"
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
            params: std::sync::Arc::new(std::collections::BTreeMap::new()),
        };
        let empty_query = engine.resolve_providers(&[], &ctx, "").await.unwrap();
        assert!(empty_query.is_empty());
        let non_empty_query = engine.resolve_providers(&[], &ctx, "foo").await.unwrap();
        assert!(non_empty_query.is_empty());
    }

    #[tokio::test]
    async fn test_resolve_providers_threads_params_per_resolution() {
        let engine = make_engine();
        let ctx = crate::providers::ProviderCtx {
            cwd: Path::new("/tmp").to_path_buf(),
            env: std::sync::Arc::new(std::collections::HashMap::new()),
            current_token: String::new(),
            params: std::sync::Arc::new(std::collections::BTreeMap::from([(
                "base".to_string(),
                "must-not-leak".to_string(),
            )])),
        };
        let first = ProviderResolution {
            kind: ProviderKind::TestEchoParams,
            params: std::sync::Arc::new(std::collections::BTreeMap::from([
                ("provider".to_string(), "first".to_string()),
                ("shared".to_string(), "one".to_string()),
            ])),
        };
        let second = ProviderResolution {
            kind: ProviderKind::TestEchoParams,
            params: std::sync::Arc::new(std::collections::BTreeMap::from([(
                "provider".to_string(),
                "second".to_string(),
            )])),
        };

        let results = engine
            .resolve_providers(&[first, second], &ctx, "")
            .await
            .unwrap();
        let texts: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();

        assert_eq!(texts, ["provider=first", "shared=one", "provider=second"]);
    }

    #[tokio::test]
    async fn test_resolve_provider_kinds_threads_params_per_resolution() {
        let engine = make_engine();
        let ctx = crate::providers::ProviderCtx {
            cwd: Path::new("/tmp").to_path_buf(),
            env: std::sync::Arc::new(std::collections::HashMap::new()),
            current_token: String::new(),
            params: std::sync::Arc::new(std::collections::BTreeMap::from([(
                "base".to_string(),
                "must-not-leak".to_string(),
            )])),
        };
        let first = ProviderResolution {
            kind: ProviderKind::TestEchoParams,
            params: std::sync::Arc::new(std::collections::BTreeMap::from([
                ("provider".to_string(), "first".to_string()),
                ("shared".to_string(), "one".to_string()),
            ])),
        };
        let second = ProviderResolution {
            kind: ProviderKind::TestEchoParams,
            params: std::sync::Arc::new(std::collections::BTreeMap::from([(
                "provider".to_string(),
                "second".to_string(),
            )])),
        };

        let results = engine
            .resolve_provider_kinds(&[first, second], &ctx, "")
            .await;
        let texts: Vec<Vec<&str>> = results
            .iter()
            .map(|(_kind, result)| {
                result
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect()
            })
            .collect();

        assert_eq!(
            texts,
            [
                vec!["provider=first", "shared=one"],
                vec!["provider=second"]
            ]
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
            lowered_from_requires_js: false,
            static_extracted_subprocess: false,
            requires_js: false,
            js_source: None,
            js_runtime: None,
            corrected_in: None,
            template: None,
            params: std::collections::BTreeMap::new(),
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
            lowered_from_requires_js: false,
            static_extracted_subprocess: false,
            requires_js: false,
            js_source: None,
            js_runtime: None,
            corrected_in: None,
            template: None,
            params: std::collections::BTreeMap::new(),
        };
        let ctx = make_ctx(Some("test"), vec!["arg1"], "", 2);
        let argv = super::resolve_script_argv(&gen, &ctx);
        assert_eq!(argv, vec!["cmd", "arg1"]);
    }

    #[test]
    fn test_post_process_requires_non_empty_script_argv() {
        use crate::specs::{JsRuntimeKind, JsRuntimeSpec};

        let runtime = Arc::new(JsRuntimeSpec {
            kind: JsRuntimeKind::PostProcess,
            source: "out => out.split('\\n')".to_string(),
            timeout_ms: None,
            allow_shell_command: false,
            self_contained: false,
        });
        let empty_script = crate::specs::GeneratorSpec {
            generator_type: None,
            script: Some(vec![]),
            script_template: None,
            transforms: vec![],
            cache: None,
            lowered_from_requires_js: false,
            static_extracted_subprocess: false,
            requires_js: true,
            js_source: None,
            js_runtime: Some(Arc::clone(&runtime)),
            corrected_in: None,
            template: None,
            params: std::collections::BTreeMap::new(),
        };
        let empty_template = crate::specs::GeneratorSpec {
            script: None,
            script_template: Some(vec![]),
            js_runtime: Some(runtime),
            ..empty_script.clone()
        };

        assert!(!super::is_supported_script_generator(&empty_script));
        assert!(!super::is_supported_script_generator(&empty_template));
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

    // End-to-end: depends on specs/git.json declaring `archive --format` suggestions [tar, zip].
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

    // End-to-end: depends on specs/tar.json declaring `c --atime-preserve` suggestions [replace, system].
    #[test]
    fn tar_atime_preserve_returns_replace_system() {
        let engine = make_engine();
        let ctx = CommandContext {
            command: Some("tar".into()),
            args: vec!["c".into(), "--atime-preserve".into()],
            current_word: String::new(),
            word_index: 3,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: Some("--atime-preserve".into()),
            in_pipe: false,
            in_redirect: false,
            quote_state: QuoteState::None,
            is_first_segment: true,
        };
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "tar c --atime-preserve ")
            .unwrap();
        let texts: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
        assert!(
            texts.contains(&"replace"),
            "expected `replace` in {texts:?}"
        );
        assert!(texts.contains(&"system"), "expected `system` in {texts:?}");
    }

    #[test]
    fn static_suggestions_coexist_with_native_generators() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_json = r#"{
            "name": "myfake",
            "args": [{
                "name": "ref",
                "generators": [{"type": "git_branches"}],
                "suggestions": ["HEAD"]
            }]
        }"#;
        std::fs::write(tmp.path().join("myfake.json"), spec_json).unwrap();

        let spec_store = SpecStore::load_from_dir(tmp.path()).unwrap().store;
        let history = HistoryProvider::from_entries(vec![]);
        let commands = CommandsProvider::from_list(vec!["myfake".into()]);
        let engine = SuggestionEngine::with_providers(spec_store, history, commands);

        let ctx = CommandContext {
            command: Some("myfake".into()),
            args: vec![],
            current_word: String::new(),
            word_index: 1,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: QuoteState::None,
            is_first_segment: true,
        };
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "myfake ")
            .unwrap();
        let texts: Vec<&str> = results.iter().map(|s| s.text.as_str()).collect();
        assert!(
            texts.contains(&"HEAD"),
            "static suggestion `HEAD` must surface alongside the git_branches generator: {texts:?}"
        );
        assert!(
            results
                .git_generators
                .contains(&crate::git::GitQueryKind::Branches),
            "git_branches generator must be dispatched alongside static suggestions: {:?}",
            results.git_generators
        );
    }

    #[test]
    fn static_suggestions_surface_past_double_dash() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec_json = r#"{
            "name": "myfake",
            "subcommands": [{"name": "sub"}],
            "options": [{"name": ["--flag"]}],
            "args": [{
                "name": "value",
                "suggestions": ["alpha", "beta"]
            }]
        }"#;
        std::fs::write(tmp.path().join("myfake.json"), spec_json).unwrap();

        let spec_store = SpecStore::load_from_dir(tmp.path()).unwrap().store;
        let history = HistoryProvider::from_entries(vec![]);
        let commands = CommandsProvider::from_list(vec!["myfake".into()]);
        let engine = SuggestionEngine::with_providers(spec_store, history, commands);

        let ctx = CommandContext {
            command: Some("myfake".into()),
            args: vec!["--".into()],
            current_word: String::new(),
            word_index: 2,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: QuoteState::None,
            is_first_segment: true,
        };
        let results = engine
            .suggest_sync(&ctx, Path::new("/tmp"), "myfake -- ")
            .unwrap();
        assert!(
            results.iter().any(|s| s.text == "alpha"),
            "static suggestion `alpha` must surface past `--`: {:?}",
            results.suggestions
        );
        assert!(
            results
                .iter()
                .all(|s| s.kind != SuggestionKind::Subcommand && s.kind != SuggestionKind::Flag),
            "no Subcommand/Flag entries must leak past `--`: {:?}",
            results
                .suggestions
                .iter()
                .map(|s| (&s.text, &s.kind))
                .collect::<Vec<_>>()
        );
    }

    /// Pins the documented invariant that Custom dispatch always folds
    /// `cwd` into the cache key, even when `cache_by_directory: false`.
    /// A future cleanup that respects `cache_by_directory` for Custom
    /// (parsing the doc as `false` → no cwd) would silently leak
    /// cross-repo branches; this test fails before that ever ships.
    #[tokio::test]
    async fn custom_cache_key_includes_cwd_even_with_cache_by_directory_false() {
        use crate::specs::{CacheConfig, GeneratorSpec, JsRuntimeKind, JsRuntimeSpec};
        use std::sync::Arc;

        // Counter-file body: each invocation appends to the file. If the
        // cache key did NOT discriminate on cwd, two calls from
        // different cwds would collide on the same slot and only one
        // script run would happen.
        let tmp = tempfile::tempdir().expect("tempdir");
        let counter = tmp.path().join("count");
        let counter_path = counter.display().to_string();
        let source = format!(
            "async (tokens, run, ctx) => {{ \
                await run(['sh', '-c', 'echo run >> {path}']); \
                return [{{ name: 'ok' }}]; \
            }}",
            path = counter_path,
        );
        let gen = Arc::new(GeneratorSpec {
            generator_type: None,
            script: None,
            script_template: None,
            transforms: Vec::new(),
            cache: Some(CacheConfig {
                ttl_seconds: 60,
                cache_by_directory: false,
            }),
            lowered_from_requires_js: false,
            static_extracted_subprocess: false,
            requires_js: true,
            js_source: None,
            js_runtime: Some(Arc::new(JsRuntimeSpec {
                kind: JsRuntimeKind::Custom,
                source,
                self_contained: true,
                timeout_ms: None,
                allow_shell_command: false,
            })),
            corrected_in: None,
            template: None,
            params: std::collections::BTreeMap::new(),
        });

        let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap().store;
        let history = HistoryProvider::from_entries(Vec::new());
        let commands = CommandsProvider::from_list(Vec::new());
        let engine = SuggestionEngine::with_providers(spec_store, history, commands);

        let cwd_a = tmp.path().join("repo-a");
        let cwd_b = tmp.path().join("repo-b");
        std::fs::create_dir_all(&cwd_a).expect("create cwd_a");
        std::fs::create_dir_all(&cwd_b).expect("create cwd_b");

        let ctx = make_ctx(Some("custom-cwd-cache-test"), Vec::new(), "", 1);

        let _first = engine
            .run_generators(std::slice::from_ref(&gen), &ctx, &cwd_a, 5_000)
            .await
            .expect("first dispatch");
        let _second = engine
            .run_generators(&[gen], &ctx, &cwd_b, 5_000)
            .await
            .expect("second dispatch");

        let counter_contents = std::fs::read_to_string(&counter).expect("counter file written");
        let runs = counter_contents.lines().count();
        assert_eq!(
            runs, 2,
            "cache key must differentiate on cwd even with cache_by_directory=false; \
             got {runs} script runs (contents: {counter_contents:?})"
        );
    }

    /// Pins the warn-and-continue contract for hand-built specs that
    /// claim `requires_js: true` but ship no `js_runtime` metadata. The
    /// branch is unreachable through `SpecStore` (loader rejects it)
    /// but reachable through hand-built in-memory specs in tests/fuzz/
    /// embedded fallback. A future refactor that mistakes the `None`
    /// arm for `unreachable!()` would panic the engine the first time
    /// such a spec slipped through.
    #[tokio::test]
    async fn requires_js_with_none_js_runtime_returns_empty_without_panicking() {
        use crate::specs::GeneratorSpec;
        use std::sync::Arc;

        let gen = Arc::new(GeneratorSpec {
            generator_type: None,
            script: None,
            script_template: None,
            transforms: Vec::new(),
            cache: None,
            lowered_from_requires_js: false,
            static_extracted_subprocess: false,
            requires_js: true,
            js_source: None,
            // The defensive None branch under test.
            js_runtime: None,
            corrected_in: None,
            template: None,
            params: std::collections::BTreeMap::new(),
        });

        let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap().store;
        let history = HistoryProvider::from_entries(Vec::new());
        let commands = CommandsProvider::from_list(Vec::new());
        let engine = SuggestionEngine::with_providers(spec_store, history, commands);

        let ctx = make_ctx(Some("requires-js-no-runtime"), Vec::new(), "", 1);
        let results = engine
            .run_generators(&[gen], &ctx, Path::new("/tmp"), 5_000)
            .await
            .expect("must not panic on hand-built malformed spec");
        assert!(
            results.is_empty(),
            "requires_js without js_runtime must yield empty results, got {results:?}"
        );
    }

    #[test]
    fn token_only_demotion_recovers_from_poisoned_mutex() {
        // Verifies the contract of `TokenOnlyDemotionState::lock_failures`:
        // a poisoned mutex (from a panicking holder) must NOT propagate as
        // a `PoisonError` to callers — `is_demoted`, `record_timeout`, and
        // `record_success` must all return without panic, and the helper
        // must wipe the (potentially torn) map so stale counts can't be
        // observed by the call site. The very first recovery invocation
        // must also clear the mutex's poison flag, so subsequent locks
        // take the `Ok` arm and the `tracing::warn!` fires exactly once
        // per poison event rather than on every keystroke for the rest
        // of the process lifetime.
        let state = Arc::new(TokenOnlyDemotionState::default());

        // Pre-poison: lock the mutex inside a thread that then panics.
        let state_clone = Arc::clone(&state);
        let join = std::thread::spawn(move || {
            let mut g = state_clone
                .consecutive_failures
                .lock()
                .expect("first lock succeeds");
            g.insert("gen-x".to_string(), 7); // completed insert; the poison flag now makes it unsafe to observe
            panic!("simulated panic to poison the mutex");
        })
        .join();
        assert!(join.is_err(), "helper thread should have panicked");
        assert!(
            state.consecutive_failures.is_poisoned(),
            "mutex must be poisoned after the helper thread panicked while holding it"
        );

        // First recovery call: must not panic, must ignore the torn count
        // (`gen-x => 7` would otherwise trigger a false demotion), and
        // must clear the poison flag as a side effect.
        assert!(
            !state.is_demoted("gen-x"),
            "is_demoted must recover from poison and ignore the torn count"
        );
        assert!(
            state.consecutive_failures.lock().is_ok(),
            "first poison-recovery call must clear the poison flag so subsequent locks succeed"
        );
        assert!(
            !state.consecutive_failures.is_poisoned(),
            "poison flag must be cleared after the first recovery invocation"
        );

        // Remaining call sites must keep working normally once the mutex
        // has recovered — they take the `Ok` arm now and behave as on a
        // fresh state.
        let count = state.record_timeout("gen-x");
        assert_eq!(
            count, 1,
            "record_timeout must start fresh after wipe, not resume from the torn value"
        );
        state.record_success("gen-x");

        // Final invariant: state observed via a plain lock (no recovery
        // needed anymore) is empty — record_success removed the entry it
        // just inserted.
        let g = state
            .consecutive_failures
            .lock()
            .expect("mutex must no longer be poisoned");
        assert!(
            g.is_empty(),
            "after recovery + record_success, the failures map must be empty, got {g:?}"
        );
    }

    // ---- engine_history: history-lane reservation ----

    fn make_history_engine(history: Vec<String>) -> SuggestionEngine {
        let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap().store;
        let history = HistoryProvider::from_entries(history);
        let commands = CommandsProvider::from_list(vec!["git".into()]);
        let mut engine = SuggestionEngine::with_providers(spec_store, history, commands);
        engine.max_results = 10;
        engine.max_history_results = 5;
        engine
    }

    fn flag_candidates(n: usize) -> Vec<Suggestion> {
        (0..n)
            .map(|i| Suggestion {
                text: format!("flag{i}"),
                kind: SuggestionKind::Flag,
                source: SuggestionSource::Spec,
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn engine_history_reserves_two_rows_when_candidates_fill_budget() {
        // With max_results = 10 and 10 candidates that all match the
        // (empty) current_word, the pre-reservation behaviour saturated
        // the popup with candidates and pushed every history row out. The
        // reservation lane shrinks the candidate cap by 2 so two high-
        // confidence (prefix-matching) history entries always have room.
        let engine = make_history_engine(vec![
            "git checkout master".into(),
            "git checkout develop".into(),
        ]);
        let ctx = make_ctx(Some("git"), vec!["checkout"], "", 2);
        let results = engine.rank_with_history(
            &ctx,
            Path::new("/tmp"),
            "git checkout ",
            flag_candidates(10),
            true,
        );

        assert_eq!(
            results.len(),
            10,
            "8 candidates + 2 reserved history must still fit max_results = 10 (no popup growth)"
        );
        let history_count = results
            .iter()
            .filter(|s| s.source == SuggestionSource::History)
            .count();
        assert_eq!(
            history_count, 2,
            "two prefix-matching history entries must survive: {results:?}"
        );
    }

    #[test]
    fn engine_history_skipped_in_redirect_context() {
        let engine = make_history_engine(vec!["echo redirected".into()]);
        let mut ctx = make_ctx(Some("echo"), vec![], "", 1);
        ctx.in_redirect = true;
        let candidates = vec![Suggestion {
            text: "foo.txt".to_string(),
            kind: SuggestionKind::FilePath,
            source: SuggestionSource::Filesystem,
            ..Default::default()
        }];
        let results =
            engine.rank_with_history(&ctx, Path::new("/tmp"), "echo > ", candidates, true);
        let history_count = results
            .iter()
            .filter(|s| s.source == SuggestionSource::History)
            .count();
        assert_eq!(
            history_count, 0,
            "redirect context expects filenames, not command history: {results:?}"
        );
    }

    #[test]
    fn engine_history_skipped_in_flag_context() {
        // ctx.is_flag = current_word.starts_with('-'). Flags don't
        // prefix-match command lines, so the lane is wasted here.
        let engine = make_history_engine(vec!["git --version".into()]);
        let ctx = make_ctx(Some("git"), vec![], "--", 1);
        let candidates = flag_candidates(10);
        let results = engine.rank_with_history(&ctx, Path::new("/tmp"), "git --", candidates, true);
        let history_count = results
            .iter()
            .filter(|s| s.source == SuggestionSource::History)
            .count();
        assert_eq!(
            history_count, 0,
            "flag context must not surface history rows: {results:?}"
        );
    }

    #[test]
    fn engine_history_no_prefix_match_preserves_full_candidate_budget() {
        // The common production path: history is non-empty but NO entry
        // prefix-matches the buffer, so reserved_history == 0 and the full
        // candidate budget (max_results) is available. A regression in the
        // prefix predicate would silently shrink this budget every keystroke.
        let engine = make_history_engine(vec!["docker build .".into(), "ls -la".into()]);
        let ctx = make_ctx(Some("git"), vec!["checkout"], "", 2);
        let results = engine.rank_with_history(
            &ctx,
            Path::new("/tmp"),
            "git checkout ",
            flag_candidates(10),
            true,
        );

        assert_eq!(
            results.len(),
            10,
            "no prefix-matching history => reserved_history == 0 => full max_results budget: {results:?}"
        );
        let history_count = results
            .iter()
            .filter(|s| s.source == SuggestionSource::History)
            .count();
        // Candidates saturate the budget, so any history can only arrive via
        // fuzzy-fill of leftover slack (there is none here).
        assert_eq!(
            history_count, 0,
            "no candidate slot was displaced for non-matching history: {results:?}"
        );
    }

    #[test]
    fn engine_history_reserves_one_row_for_single_prefix_match() {
        // ONE prefix-matching entry => reserved_history == 1 =>
        // normal_budget == max_results - 1, then the entry fuzzy-fills the
        // single freed slot.
        let engine = make_history_engine(vec!["git checkout main".into()]);
        let ctx = make_ctx(Some("git"), vec!["checkout"], "", 2);
        let results = engine.rank_with_history(
            &ctx,
            Path::new("/tmp"),
            "git checkout ",
            flag_candidates(10),
            true,
        );

        assert_eq!(
            results.len(),
            10,
            "9 candidates + 1 reserved history must fit max_results = 10: {results:?}"
        );
        let history_count = results
            .iter()
            .filter(|s| s.source == SuggestionSource::History)
            .count();
        assert_eq!(
            history_count, 1,
            "the single prefix-matching entry must survive: {results:?}"
        );
    }

    #[test]
    fn engine_history_clamps_reservation_to_two_with_three_prefix_matches() {
        // THREE prefix-matching entries: `.take(RESERVED_HISTORY)` must clamp
        // the reservation to 2, so candidate_count == 8. The extra match may
        // still fuzzy-fill slack, but it must not reserve a third slot.
        let engine = make_history_engine(vec![
            "git checkout master".into(),
            "git checkout develop".into(),
            "git checkout main".into(),
        ]);
        let ctx = make_ctx(Some("git"), vec!["checkout"], "", 2);
        let results = engine.rank_with_history(
            &ctx,
            Path::new("/tmp"),
            "git checkout ",
            flag_candidates(10),
            true,
        );

        assert_eq!(
            results.len(),
            10,
            "8 candidates + 2 reserved history must fit max_results = 10: {results:?}"
        );
        let candidate_count = results
            .iter()
            .filter(|s| s.source != SuggestionSource::History)
            .count();
        assert_eq!(
            candidate_count, 8,
            "reservation must be clamped to 2 (.take(RESERVED_HISTORY)), not 3: {results:?}"
        );
    }

    #[test]
    fn engine_history_clamps_reservation_to_max_history_results() {
        // Exercises the `.min(self.max_history_results)` clamp on the
        // reservation count. With max_history_results = 1 and TWO prefix-
        // matching entries, `.take(RESERVED_HISTORY)` counts 2, but the clamp
        // must reduce reserved_history to 1 — otherwise normal_budget shrinks
        // by 2 while only 1 history row is ever appended, wasting a popup slot.
        let engine = {
            let mut engine = make_history_engine(vec![
                "git checkout master".into(),
                "git checkout develop".into(),
            ]);
            engine.max_history_results = 1;
            engine
        };
        let ctx = make_ctx(Some("git"), vec!["checkout"], "", 2);
        let results = engine.rank_with_history(
            &ctx,
            Path::new("/tmp"),
            "git checkout ",
            flag_candidates(10),
            true,
        );

        assert_eq!(
            results.len(),
            10,
            "reservation clamped to max_history_results = 1 => 9 candidates + 1 history fills \
             max_results = 10 with no wasted slot: {results:?}"
        );
        let history_count = results
            .iter()
            .filter(|s| s.source == SuggestionSource::History)
            .count();
        assert_eq!(
            history_count, 1,
            "max_history_results = 1 caps history to a single row even with two prefix \
             matches (reserved_history clamped to 1, not 2): {results:?}"
        );
        let candidate_count = results
            .iter()
            .filter(|s| s.source != SuggestionSource::History)
            .count();
        assert_eq!(
            candidate_count, 9,
            "without the .min() clamp, normal_budget would drop to 8 and leave only 8 \
             candidate rows; the clamp keeps 9: {results:?}"
        );
    }

    #[test]
    fn engine_history_empty_provider_with_allowed_lane_preserves_budget() {
        // Empty history while the lane is ALLOWED (not redirect/flag): the
        // false arm of `if !history_entries.is_empty()` and the
        // `saturating_sub` reservation math must not panic or deduct slots.
        let engine = make_history_engine(vec![]);
        let ctx = make_ctx(Some("git"), vec![], "", 1);
        let results =
            engine.rank_with_history(&ctx, Path::new("/tmp"), "git ", flag_candidates(3), true);

        assert_eq!(
            results.len(),
            3,
            "empty history reserves nothing => full candidate budget: {results:?}"
        );
        let history_count = results
            .iter()
            .filter(|s| s.source == SuggestionSource::History)
            .count();
        assert_eq!(
            history_count, 0,
            "empty history provider yields no history rows: {results:?}"
        );
    }
}
