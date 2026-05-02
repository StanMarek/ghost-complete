//! Native providers — async, context-aware suggestion sources that
//! replace JavaScript-backed Fig generators for a curated set of
//! commands.
//!
//! This module is the scaffolding counterpart to `crate::git`:
//! - `Provider` is the async trait every native provider implements.
//! - `ProviderCtx` is the context handed to each `generate` call (cwd,
//!   environment, current token).
//! - `ProviderKind` is a closed-for-this-crate enum listing every
//!   registered provider. Adding a new provider means adding one
//!   variant + one `kind_from_type_str` arm + one `resolve` arm.
//! - `kind_from_type_str` is the string→kind dispatcher wired up from
//!   spec loading. Specs reference providers via `{"type": "<name>"}`
//!   exactly like the existing `git_branches` / `filepaths` native
//!   generator types.
//! - `resolve` is the per-kind dispatcher called by
//!   `SuggestionEngine::resolve_providers`.
//!
//! Note: the sync `crate::provider::Provider` trait (singular module
//! name) is the unrelated top-level source trait used by
//! `CommandsProvider`, `EnvProvider`, `FilesystemProvider`, and
//! `HistoryProvider`. The two traits coexist by sitting in different
//! modules; do not confuse them.
//!
//! ### Async trait encoding
//!
//! We cannot use native `async fn` in traits with `dyn Provider` on
//! stable Rust, and we deliberately avoid adding the `async-trait`
//! crate as a new dependency. Instead, `generate` returns an explicit
//! `impl Future<Output = Result<Vec<Suggestion>>> + Send` — each
//! implementer writes `async fn generate(...)` which desugars to the
//! same signature. The per-kind dispatch in `resolve` matches on the
//! enum and awaits the concrete provider directly, which avoids needing
//! `dyn` at all.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

use crate::types::Suggestion;

pub mod ansible_doc;
pub mod arduino_cli;
pub mod local_project;
pub mod macos_defaults;
pub mod mamba;
pub mod multipass;
pub mod pandoc;

/// Context passed to every provider's `generate` call. Owned by the
/// engine; providers receive it by reference so the shared env map is
/// not cloned per invocation.
///
/// **Invariant:** `cwd` is expected to be an absolute path. Providers
/// (`find_cargo_root`, `find_makefile`, `find_package_json`) walk
/// `cwd` ancestors assuming an absolute path — a relative cwd silently
/// produces nonsensical ancestor walks. New code SHOULD construct via
/// [`ProviderCtx::new`], which validates the invariant. The fields
/// remain `pub` for backwards compatibility with existing in-tree
/// callers (engine, gc-pty, provider unit tests); a future refactor
/// may downgrade them to `pub(crate)` once those call sites migrate.
/// While direct construction remains possible,
/// `SuggestionEngine::resolve_providers` also enforces the invariant at
/// the provider dispatch boundary.
pub struct ProviderCtx {
    /// Working directory the shell was in when the completion trigger
    /// fired. Providers that shell out to external tools pass this as
    /// the subprocess cwd. MUST be an absolute path — see struct docs.
    pub cwd: PathBuf,
    /// Snapshot of the shell's environment at trigger time. `Arc`
    /// because the engine hands the same map to every provider in a
    /// single resolution pass.
    pub env: Arc<HashMap<String, String>>,
    /// The partially-typed token the user is currently completing. May
    /// be empty when the trigger fires on a space after a subcommand.
    pub current_token: String,
}

/// Errors produced when constructing a [`ProviderCtx`] via
/// [`ProviderCtx::new`].
#[derive(Debug)]
pub enum CtxError {
    /// `cwd` was a relative path. Providers walk `cwd` ancestors and
    /// rely on an absolute root; a relative cwd silently produces
    /// nonsensical ancestor walks.
    RelativeCwd(PathBuf),
}

impl std::fmt::Display for CtxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RelativeCwd(p) => write!(
                f,
                "ProviderCtx requires an absolute cwd, got relative path: {}",
                p.display()
            ),
        }
    }
}

impl std::error::Error for CtxError {}

impl ProviderCtx {
    /// Construct a [`ProviderCtx`], rejecting a relative `cwd`. This
    /// is the preferred entry point for new call sites; existing
    /// callers continue to mint the struct directly via the public
    /// fields until they migrate.
    pub fn new(
        cwd: PathBuf,
        env: Arc<HashMap<String, String>>,
        current_token: String,
    ) -> Result<Self, CtxError> {
        if !cwd.is_absolute() {
            return Err(CtxError::RelativeCwd(cwd));
        }
        Ok(Self {
            cwd,
            env,
            current_token,
        })
    }

    /// Test-only constructor that bypasses the absolute-cwd check.
    /// Lets unit tests construct a `ProviderCtx` from a relative or
    /// otherwise-synthetic path without tripping the validation in
    /// [`Self::new`]. Available within the crate only.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn new_for_test(
        cwd: PathBuf,
        env: Arc<HashMap<String, String>>,
        current_token: String,
    ) -> Self {
        Self {
            cwd,
            env,
            current_token,
        }
    }
}

/// Async source of `Suggestion`s driven by a `{"type": "<name>"}`
/// generator in a completion spec.
///
/// Returning `impl Future + Send` (rather than `async fn`) is
/// deliberate — see the module-level docs for the full rationale. Each
/// implementer writes a normal `async fn generate(&self, ctx:
/// &ProviderCtx) -> Result<Vec<Suggestion>>` body; the compiler
/// desugars it into the required impl-trait signature.
pub trait Provider: Send + Sync {
    /// Stable identifier for this provider. Must match the `"type"`
    /// string used in JSON specs and the arm added to
    /// `kind_from_type_str` so dispatch is total.
    fn name(&self) -> &'static str;

    /// Produce suggestions for the given context.
    ///
    /// Implementations MUST NOT panic and MUST NOT propagate errors
    /// that could stall completion — the engine wraps each call in
    /// `tracing::warn!` + empty-vec fallback (matching the `git.rs`
    /// pattern), but providers are still responsible for applying
    /// their own timeouts on external calls.
    fn generate(
        &self,
        ctx: &ProviderCtx,
    ) -> impl std::future::Future<Output = Result<Vec<Suggestion>>> + Send;
}

/// Registered native providers. Closed inside this crate — every
/// production variant is listed below — but marked `#[non_exhaustive]`
/// so downstream crates cannot rely on exhaustive matches and we can
/// add a provider without breaking them on a patch release. Adding a
/// variant requires matching arms in `kind_from_type_str` and
/// `resolve`; both are dispatched from `SuggestionEngine`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProviderKind {
    /// `ansible-doc --list --json`, projecting each key (fully
    /// qualified module name) of the top-level JSON object with its
    /// short description as the suggestion description.
    AnsibleDocModules,
    /// `arduino-cli board list --format json`, projecting the first
    /// matching board's FQBN out of each detected port entry.
    ArduinoCliBoards,
    /// `arduino-cli board list --format json`, projecting `port.address`
    /// out of each entry that has at least one matching board.
    ArduinoCliPorts,
    /// Workspace member package names from the nearest ancestor
    /// `Cargo.toml`. Falls back to the single `package.name` when the
    /// manifest has no `[workspace]` table — keeps `cargo run -p
    /// <NAME>` completing in single-package crates.
    CargoWorkspaceMembers,
    /// `defaults domains`, splitting the single-line comma-separated
    /// output into individual macOS preference domain identifiers.
    DefaultsDomains,
    /// Targets parsed from the nearest ancestor
    /// `GNUmakefile`/`makefile`/`Makefile`. Hand-parsed (no `make -qp`
    /// shellout). Filters meta targets, pattern rules, and
    /// variable-expanded targets — see
    /// [`local_project::makefile::parse_makefile_targets`] for the
    /// full filter set.
    MakefileTargets,
    /// `conda env list`, projecting the first whitespace-delimited
    /// token of each data row (the env name). Used by the mamba spec,
    /// which wraps conda's CLI.
    MambaEnvs,
    /// `multipass list --format=json`, projecting the `name` field of
    /// each entry in the top-level `list` array.
    MultipassList,
    /// Keys of the `scripts` object in the nearest ancestor
    /// `package.json`. Description is the script value, truncated to
    /// 120 characters. Does not honour `package.json#fig.scripts`
    /// overrides — that's a v2 concern.
    NpmScripts,
    /// Multipass instances excluding rows in the `Deleted` state.
    MultipassListNotDeleted,
    /// Multipass instances only in the `Deleted` state.
    MultipassListDeleted,
    /// Multipass instances only in the `Running` state.
    MultipassListRunning,
    /// Multipass instances only in the `Stopped` state.
    MultipassListStopped,
    /// `pandoc --list-input-formats`, emitting one format identifier
    /// per non-empty line.
    PandocInputFormats,
    /// `pandoc --list-output-formats`, emitting one format identifier
    /// per non-empty line.
    PandocOutputFormats,
}

impl ProviderKind {
    /// Every registered provider variant in declaration order. The
    /// single source of truth for the variant set used by
    /// [`kind_from_type_str`] (string→kind dispatch). Adding a variant
    /// to `ProviderKind` requires adding it here AND to [`resolve`];
    /// the test `test_kind_from_type_str_known_providers` pins the
    /// string contract for each entry.
    pub const ALL: &'static [ProviderKind] = &[
        ProviderKind::AnsibleDocModules,
        ProviderKind::ArduinoCliBoards,
        ProviderKind::ArduinoCliPorts,
        ProviderKind::CargoWorkspaceMembers,
        ProviderKind::DefaultsDomains,
        ProviderKind::MakefileTargets,
        ProviderKind::MambaEnvs,
        ProviderKind::MultipassList,
        ProviderKind::MultipassListNotDeleted,
        ProviderKind::MultipassListDeleted,
        ProviderKind::MultipassListRunning,
        ProviderKind::MultipassListStopped,
        ProviderKind::NpmScripts,
        ProviderKind::PandocInputFormats,
        ProviderKind::PandocOutputFormats,
    ];

    /// The stable `"type"` string for this provider — the same string
    /// that appears in JSON specs and that [`kind_from_type_str`]
    /// matches against. Single source of truth: `Provider::name(&self)`
    /// impls return the same string by hand-coded literal today, but
    /// new code should prefer `kind.type_str()` so a future variant
    /// rename has one place to change.
    pub const fn type_str(self) -> &'static str {
        match self {
            Self::AnsibleDocModules => "ansible_doc_modules",
            Self::ArduinoCliBoards => "arduino_cli_boards",
            Self::ArduinoCliPorts => "arduino_cli_ports",
            Self::CargoWorkspaceMembers => "cargo_workspace_members",
            Self::DefaultsDomains => "defaults_domains",
            Self::MakefileTargets => "makefile_targets",
            Self::MambaEnvs => "mamba_envs",
            Self::MultipassList => "multipass_list",
            Self::MultipassListNotDeleted => "multipass_list_not_deleted",
            Self::MultipassListDeleted => "multipass_list_deleted",
            Self::MultipassListRunning => "multipass_list_running",
            Self::MultipassListStopped => "multipass_list_stopped",
            Self::NpmScripts => "npm_scripts",
            Self::PandocInputFormats => "pandoc_input_formats",
            Self::PandocOutputFormats => "pandoc_output_formats",
        }
    }
}

/// Map a spec's `"type"` string to a `ProviderKind`, or `None` if the
/// string does not name a registered native provider.
///
/// This is the single source of truth wired into
/// `specs::collect_generators`: when a `GeneratorSpec.generator_type`
/// returns `Some(kind)` here, the spec resolution routes it into
/// `provider_generators` instead of the script path. Iterates
/// [`ProviderKind::ALL`] and matches against [`ProviderKind::type_str`]
/// so adding a new provider only requires a new variant, an `ALL`
/// entry, and a `type_str` arm— there is no separate string→kind table to keep
/// in sync.
pub fn kind_from_type_str(type_str: &str) -> Option<ProviderKind> {
    ProviderKind::ALL
        .iter()
        .find(|kind| kind.type_str() == type_str)
        .copied()
}

/// Dispatch a single provider kind against `ctx`. The engine iterates
/// the slice of kinds from a `SpecResolution` and awaits each.
pub async fn resolve(kind: ProviderKind, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
    match kind {
        ProviderKind::AnsibleDocModules => ansible_doc::AnsibleDocModules.generate(ctx).await,
        ProviderKind::ArduinoCliBoards => arduino_cli::ArduinoCliBoards.generate(ctx).await,
        ProviderKind::ArduinoCliPorts => arduino_cli::ArduinoCliPorts.generate(ctx).await,
        ProviderKind::CargoWorkspaceMembers => {
            local_project::cargo_workspace::CargoWorkspaceMembers
                .generate(ctx)
                .await
        }
        ProviderKind::DefaultsDomains => macos_defaults::DefaultsDomains.generate(ctx).await,
        ProviderKind::MakefileTargets => {
            local_project::makefile::MakefileTargets.generate(ctx).await
        }
        ProviderKind::MambaEnvs => mamba::MambaEnvs.generate(ctx).await,
        ProviderKind::MultipassList => multipass::MultipassList.generate(ctx).await,
        ProviderKind::MultipassListNotDeleted => {
            multipass::MultipassList
                .generate_with_filter(ctx, multipass::MultipassInstanceFilter::NotDeleted)
                .await
        }
        ProviderKind::MultipassListDeleted => {
            multipass::MultipassList
                .generate_with_filter(ctx, multipass::MultipassInstanceFilter::Deleted)
                .await
        }
        ProviderKind::MultipassListRunning => {
            multipass::MultipassList
                .generate_with_filter(ctx, multipass::MultipassInstanceFilter::Running)
                .await
        }
        ProviderKind::MultipassListStopped => {
            multipass::MultipassList
                .generate_with_filter(ctx, multipass::MultipassInstanceFilter::Stopped)
                .await
        }
        ProviderKind::NpmScripts => local_project::npm_scripts::NpmScripts.generate(ctx).await,
        ProviderKind::PandocInputFormats => pandoc::PandocInputFormats.generate(ctx).await,
        ProviderKind::PandocOutputFormats => pandoc::PandocOutputFormats.generate(ctx).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kind_from_type_str_unknown_returns_none() {
        // Exercises the catchall arm of the string→kind dispatcher. Any
        // string that is NOT a registered provider must return None so
        // `collect_generators` falls through to the existing unknown-type
        // warn path rather than incorrectly routing the generator to the
        // provider pipeline.
        assert!(kind_from_type_str("").is_none());
        assert!(kind_from_type_str("git_branches").is_none());
        assert!(kind_from_type_str("nonexistent_provider").is_none());
        assert!(kind_from_type_str("filepaths").is_none());
    }

    #[test]
    fn test_kind_from_type_str_known_providers() {
        // Locks in the string contract for each registered provider —
        // converter output and runtime dispatch must agree on the exact
        // spelling.
        assert_eq!(
            kind_from_type_str("ansible_doc_modules"),
            Some(ProviderKind::AnsibleDocModules)
        );
        assert_eq!(
            kind_from_type_str("arduino_cli_boards"),
            Some(ProviderKind::ArduinoCliBoards)
        );
        assert_eq!(
            kind_from_type_str("arduino_cli_ports"),
            Some(ProviderKind::ArduinoCliPorts)
        );
        assert_eq!(
            kind_from_type_str("cargo_workspace_members"),
            Some(ProviderKind::CargoWorkspaceMembers)
        );
        assert_eq!(
            kind_from_type_str("defaults_domains"),
            Some(ProviderKind::DefaultsDomains)
        );
        assert_eq!(
            kind_from_type_str("makefile_targets"),
            Some(ProviderKind::MakefileTargets)
        );
        assert_eq!(
            kind_from_type_str("npm_scripts"),
            Some(ProviderKind::NpmScripts)
        );
        assert_eq!(
            kind_from_type_str("mamba_envs"),
            Some(ProviderKind::MambaEnvs)
        );
        assert_eq!(
            kind_from_type_str("multipass_list"),
            Some(ProviderKind::MultipassList)
        );
        assert_eq!(
            kind_from_type_str("multipass_list_not_deleted"),
            Some(ProviderKind::MultipassListNotDeleted)
        );
        assert_eq!(
            kind_from_type_str("multipass_list_deleted"),
            Some(ProviderKind::MultipassListDeleted)
        );
        assert_eq!(
            kind_from_type_str("multipass_list_running"),
            Some(ProviderKind::MultipassListRunning)
        );
        assert_eq!(
            kind_from_type_str("multipass_list_stopped"),
            Some(ProviderKind::MultipassListStopped)
        );
        assert_eq!(
            kind_from_type_str("pandoc_input_formats"),
            Some(ProviderKind::PandocInputFormats)
        );
        assert_eq!(
            kind_from_type_str("pandoc_output_formats"),
            Some(ProviderKind::PandocOutputFormats)
        );
    }

    #[test]
    fn test_provider_ctx_is_constructible() {
        // Sanity: ProviderCtx fields are public and the struct is usable
        // from downstream call sites (engine + provider tests). This is
        // the minimum contract the scaffolding owes its consumers.
        let ctx = ProviderCtx {
            cwd: PathBuf::from("/tmp"),
            env: Arc::new(HashMap::new()),
            current_token: String::new(),
        };
        assert_eq!(ctx.cwd, PathBuf::from("/tmp"));
        assert!(ctx.env.is_empty());
        assert!(ctx.current_token.is_empty());
    }

    #[test]
    fn test_provider_ctx_new_accepts_absolute_cwd() {
        // The validating constructor must accept an absolute path and
        // round-trip the supplied fields unchanged.
        let res = ProviderCtx::new(
            PathBuf::from("/tmp"),
            Arc::new(HashMap::new()),
            "tok".to_string(),
        );
        let ctx = match res {
            Ok(ctx) => ctx,
            Err(e) => panic!("absolute cwd should be accepted, got {e}"),
        };
        assert_eq!(ctx.cwd, PathBuf::from("/tmp"));
        assert_eq!(ctx.current_token, "tok");
    }

    #[test]
    fn test_provider_ctx_new_rejects_relative_cwd() {
        // A relative cwd silently breaks ancestor walks in
        // find_cargo_root / find_makefile / find_package_json. The
        // constructor MUST refuse it so validation lives in one place
        // rather than every provider re-checking on entry. Avoid
        // `.expect_err(...)` here so the test does not require
        // `ProviderCtx: Debug` (which would force an extra derive on
        // a struct that never needs printing in production).
        let res = ProviderCtx::new(
            PathBuf::from("relative/dir"),
            Arc::new(HashMap::new()),
            String::new(),
        );
        match res {
            Ok(_) => panic!("relative cwd should be rejected"),
            Err(CtxError::RelativeCwd(p)) => assert_eq!(p, PathBuf::from("relative/dir")),
        }
    }

    #[test]
    fn test_provider_kind_type_str_round_trips_for_all_variants() {
        // Every entry in ProviderKind::ALL must map to a non-empty
        // type string AND that string must round-trip back to the same
        // variant via kind_from_type_str. This is the regression guard
        // against a silent variant↔string drift if a future refactor
        // edits one without the other.
        for kind in ProviderKind::ALL {
            let s = kind.type_str();
            assert!(!s.is_empty(), "type_str must not be empty for {kind:?}");
            assert_eq!(
                kind_from_type_str(s),
                Some(*kind),
                "round-trip failed for {kind:?} (type_str = {s:?})"
            );
        }
    }
}
