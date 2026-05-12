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
//!   variant + one `ProviderKind::ALL` entry + one
//!   `ProviderKind::type_str()` arm + one `resolve` arm.
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

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

use crate::types::Suggestion;

pub mod ansible_doc;
pub mod arduino_cli;
pub mod aws_profile_names;
pub mod aws_sdk;
pub mod brew;
pub mod cargo_metadata;
pub mod docker;
pub mod dscl_principals;
pub mod kubectl;
pub mod local_project;
pub mod macos_defaults;
pub mod mamba;
pub mod multipass;
pub mod npm_local;
pub mod pandoc;
pub mod systemd_units;
pub mod tmux_state;
pub mod util;
pub mod version_probe;

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
/// [`resolve`] also enforces the invariant at the provider dispatch
/// boundary.
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
    /// Generator-spec parameters resolved from the spec's `params`
    /// field. Empty for providers that do not consume them. `BTreeMap`
    /// gives deterministic iteration order; [`Self::params_hash`] is
    /// suitable only for in-process cache keys.
    ///
    /// Read by spec-driven providers (e.g. the planned `AwsSdk`
    /// provider in ux-13/14). Existing native providers ignore this
    /// field; the channel is purely additive.
    pub params: Arc<BTreeMap<String, String>>,
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
            params: Arc::new(BTreeMap::new()),
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
            params: Arc::new(BTreeMap::new()),
        }
    }

    /// Clone this context for a single provider resolution, replacing
    /// only `params` with the map declared on that resolution's source
    /// generator.
    pub fn for_resolution(&self, resolution: &ProviderResolution) -> Self {
        Self {
            cwd: self.cwd.clone(),
            env: Arc::clone(&self.env),
            current_token: self.current_token.clone(),
            params: Arc::clone(&resolution.params),
        }
    }

    /// Stable hash of [`Self::params`] suitable for in-process cache
    /// keys. `BTreeMap` iteration order is deterministic, and
    /// [`std::collections::hash_map::DefaultHasher`] is fixed-seed,
    /// so the result is stable across calls within a process. Cross-
    /// process stability is NOT guaranteed and not required — this
    /// hash exists for the in-process generator cache only.
    pub fn params_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for (k, v) in self.params.iter() {
            k.hash(&mut hasher);
            v.hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Return an environment value captured for this completion request.
    pub fn env(&self, name: &str) -> Option<&str> {
        self.env.get(name).map(String::as_str)
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
    /// string used in JSON specs and the corresponding
    /// [`ProviderKind::type_str`] result so dispatch is total.
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
/// variant requires adding it to [`ProviderKind::ALL`],
/// [`ProviderKind::type_str`], and [`resolve`]; these are dispatched
/// from `SuggestionEngine`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProviderKind {
    /// AWS SDK-backed provider for IAM list operations.
    AwsSdk,
    /// AWS profile names from shared AWS config and credentials files.
    AwsProfileNames,
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
    /// Target names from `cargo metadata --format-version 1 --no-deps`,
    /// filtered by `ctx.params["kind"]` (`bin`, `example`, `test`,
    /// `bench`, or `lib`).
    CargoTargets,
    /// Feature names from the active package in `cargo metadata
    /// --format-version 1 --no-deps`.
    CargoFeatures,
    /// `defaults domains`, splitting the single-line comma-separated
    /// output into individual macOS preference domain identifiers.
    DefaultsDomains,
    /// Docker image references from `docker images --format '{{json .}}'`.
    DockerImages,
    /// Docker containers from `docker ps -a --format '{{json .}}'`.
    DockerContainers,
    /// Running Docker containers from `docker ps --filter status=running`.
    DockerRunningContainers,
    /// Docker networks from `docker network ls`.
    DockerNetworks,
    /// Docker volumes from `docker volume ls`.
    DockerVolumes,
    /// macOS directory-service user principals from `dscl . list /Users`.
    DsclUsers,
    /// macOS directory-service group principals from `dscl . list /Groups`.
    DsclGroups,
    /// Kubernetes resource type names from `kubectl api-resources`.
    K8sResources,
    /// Kubernetes pod names from `kubectl get pods -o json`.
    K8sPods,
    /// Kubernetes namespace names from `kubectl get namespaces -o json`.
    K8sNamespaces,
    /// Kubernetes contexts from `kubectl config get-contexts -o name`.
    K8sContexts,
    /// Kubernetes node names from `kubectl get nodes -o json`.
    K8sNodes,
    /// Kubernetes service names from `kubectl get services -o json`.
    K8sServices,
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
    /// Keys of `package.json#dependencies` from the nearest ancestor
    /// `package.json`.
    NpmDependencies,
    /// Keys of `package.json#devDependencies` from the nearest ancestor
    /// `package.json`.
    NpmDevDependencies,
    /// Union of `package.json#dependencies` and `#devDependencies`.
    NpmAllDependencies,
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
    /// tmux sessions from `tmux list-sessions`.
    TmuxSessions,
    /// tmux windows from `tmux list-windows`.
    TmuxWindows,
    /// tmux panes from `tmux list-panes`.
    TmuxPanes,
    /// tmux clients from `tmux list-clients`.
    TmuxClients,
    /// systemd units from `systemctl list-units`.
    SystemdUnits,
    /// user-scoped systemd units.
    SystemdUserUnits,
    /// active systemd units.
    SystemdActiveUnits,
    /// installed Homebrew formulae.
    BrewFormulaeInstalled,
    /// installed Homebrew casks.
    BrewCasksInstalled,
    /// searchable Homebrew formulae, capped for popup latency.
    BrewFormulaeSearchable,
    /// Test-only provider that echoes `ProviderCtx::params` into
    /// suggestions so engine-boundary tests can prove per-resolution
    /// params reached dispatch.
    #[cfg(test)]
    TestEchoParams,
}

impl ProviderKind {
    /// Every registered provider variant in declaration order. The
    /// single source of truth for the variant set used by
    /// [`kind_from_type_str`] (string→kind dispatch). Adding a variant
    /// to `ProviderKind` requires adding it here, to [`Self::type_str`],
    /// AND to [`resolve`];
    /// the test `test_kind_from_type_str_known_providers` pins the
    /// string contract for each entry.
    pub const ALL: &'static [ProviderKind] = &[
        ProviderKind::AwsSdk,
        ProviderKind::AwsProfileNames,
        ProviderKind::AnsibleDocModules,
        ProviderKind::ArduinoCliBoards,
        ProviderKind::ArduinoCliPorts,
        ProviderKind::CargoWorkspaceMembers,
        ProviderKind::CargoFeatures,
        ProviderKind::CargoTargets,
        ProviderKind::BrewCasksInstalled,
        ProviderKind::BrewFormulaeInstalled,
        ProviderKind::BrewFormulaeSearchable,
        ProviderKind::DefaultsDomains,
        ProviderKind::DockerContainers,
        ProviderKind::DockerImages,
        ProviderKind::DockerNetworks,
        ProviderKind::DockerRunningContainers,
        ProviderKind::DockerVolumes,
        ProviderKind::DsclGroups,
        ProviderKind::DsclUsers,
        ProviderKind::K8sContexts,
        ProviderKind::K8sNamespaces,
        ProviderKind::K8sNodes,
        ProviderKind::K8sPods,
        ProviderKind::K8sResources,
        ProviderKind::K8sServices,
        ProviderKind::MakefileTargets,
        ProviderKind::MambaEnvs,
        ProviderKind::MultipassList,
        ProviderKind::MultipassListNotDeleted,
        ProviderKind::MultipassListDeleted,
        ProviderKind::MultipassListRunning,
        ProviderKind::MultipassListStopped,
        ProviderKind::NpmAllDependencies,
        ProviderKind::NpmDependencies,
        ProviderKind::NpmDevDependencies,
        ProviderKind::NpmScripts,
        ProviderKind::PandocInputFormats,
        ProviderKind::PandocOutputFormats,
        ProviderKind::SystemdActiveUnits,
        ProviderKind::SystemdUnits,
        ProviderKind::SystemdUserUnits,
        ProviderKind::TmuxClients,
        ProviderKind::TmuxPanes,
        ProviderKind::TmuxSessions,
        ProviderKind::TmuxWindows,
    ];

    /// The stable `"type"` string for this provider — the same string
    /// that appears in JSON specs and that [`kind_from_type_str`]
    /// matches against. Single source of truth: `Provider::name(&self)`
    /// impls return the same string by hand-coded literal today, but
    /// new code should prefer `kind.type_str()` so a future variant
    /// rename has one place to change.
    pub const fn type_str(self) -> &'static str {
        match self {
            Self::AwsSdk => "aws_sdk",
            Self::AwsProfileNames => "aws_profile_names",
            Self::AnsibleDocModules => "ansible_doc_modules",
            Self::ArduinoCliBoards => "arduino_cli_boards",
            Self::ArduinoCliPorts => "arduino_cli_ports",
            Self::CargoWorkspaceMembers => "cargo_workspace_members",
            Self::CargoTargets => "cargo_targets",
            Self::CargoFeatures => "cargo_features",
            Self::BrewCasksInstalled => "brew_casks_installed",
            Self::BrewFormulaeInstalled => "brew_formulae_installed",
            Self::BrewFormulaeSearchable => "brew_formulae_searchable",
            Self::DefaultsDomains => "defaults_domains",
            Self::DockerContainers => "docker_containers",
            Self::DockerImages => "docker_images",
            Self::DockerNetworks => "docker_networks",
            Self::DockerRunningContainers => "docker_running_containers",
            Self::DockerVolumes => "docker_volumes",
            Self::DsclGroups => "dscl_groups",
            Self::DsclUsers => "dscl_users",
            Self::K8sContexts => "k8s_contexts",
            Self::K8sNamespaces => "k8s_namespaces",
            Self::K8sNodes => "k8s_nodes",
            Self::K8sPods => "k8s_pods",
            Self::K8sResources => "k8s_resources",
            Self::K8sServices => "k8s_services",
            Self::MakefileTargets => "makefile_targets",
            Self::MambaEnvs => "mamba_envs",
            Self::MultipassList => "multipass_list",
            Self::MultipassListNotDeleted => "multipass_list_not_deleted",
            Self::MultipassListDeleted => "multipass_list_deleted",
            Self::MultipassListRunning => "multipass_list_running",
            Self::MultipassListStopped => "multipass_list_stopped",
            Self::NpmAllDependencies => "npm_all_dependencies",
            Self::NpmDependencies => "npm_dependencies",
            Self::NpmDevDependencies => "npm_dev_dependencies",
            Self::NpmScripts => "npm_scripts",
            Self::PandocInputFormats => "pandoc_input_formats",
            Self::PandocOutputFormats => "pandoc_output_formats",
            Self::SystemdActiveUnits => "systemd_active_units",
            Self::SystemdUnits => "systemd_units",
            Self::SystemdUserUnits => "systemd_user_units",
            Self::TmuxClients => "tmux_clients",
            Self::TmuxPanes => "tmux_panes",
            Self::TmuxSessions => "tmux_sessions",
            Self::TmuxWindows => "tmux_windows",
            #[cfg(test)]
            Self::TestEchoParams => "__test_echo_params",
        }
    }
}

/// One native provider scheduled by spec resolution, paired with the
/// `params` map declared on the source [`crate::specs::GeneratorSpec`].
///
/// Replaces the bare `Vec<ProviderKind>` carried through
/// `SpecResolution.provider_generators` so the engine can build a
/// per-provider [`ProviderCtx`] whose `params` are set to the spec's
/// declared values. Existing native providers ignore `params`; future
/// spec-driven providers (`AwsSdk`, etc.) read them via `ctx.params`.
///
/// `params` lives behind an `Arc<BTreeMap<...>>` so cloning the
/// resolution is a refcount bump on the keystroke hot path. Stable
/// `BTreeMap` iteration order gives deterministic hashing input;
/// [`ProviderCtx::params_hash`] remains an in-process cache key only.
#[derive(Debug, Clone)]
pub struct ProviderResolution {
    pub kind: ProviderKind,
    pub params: Arc<BTreeMap<String, String>>,
}

impl ProviderResolution {
    /// Convenience constructor for callers that only have a kind on
    /// hand and don't need to thread params yet — the empty-params
    /// case stays a one-liner.
    pub fn from_kind(kind: ProviderKind) -> Self {
        Self {
            kind,
            params: Arc::new(BTreeMap::new()),
        }
    }
}

impl From<ProviderKind> for ProviderResolution {
    fn from(kind: ProviderKind) -> Self {
        Self::from_kind(kind)
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
/// entry, and a `type_str` arm — there is no separate string→kind table
/// to keep in sync.
pub fn kind_from_type_str(type_str: &str) -> Option<ProviderKind> {
    ProviderKind::ALL
        .iter()
        .find(|kind| kind.type_str() == type_str)
        .copied()
}

/// Dispatch a single provider kind against `ctx`. The engine iterates
/// the slice of kinds from a `SpecResolution` and awaits each. Rejects
/// relative cwd values at the shared provider boundary so direct
/// callers cannot bypass [`ProviderCtx::new`] and accidentally make
/// local-project providers walk ancestors from the process cwd.
pub async fn resolve(kind: ProviderKind, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
    if !ctx.cwd.is_absolute() {
        tracing::warn!(
            cwd = %ctx.cwd.display(),
            "provider cwd is relative; skipping provider resolution"
        );
        return Ok(Vec::new());
    }

    match kind {
        ProviderKind::AwsSdk => aws_sdk::AwsSdk.generate(ctx).await,
        ProviderKind::AwsProfileNames => aws_profile_names::AwsProfileNames.generate(ctx).await,
        ProviderKind::AnsibleDocModules => ansible_doc::AnsibleDocModules.generate(ctx).await,
        ProviderKind::ArduinoCliBoards => arduino_cli::ArduinoCliBoards.generate(ctx).await,
        ProviderKind::ArduinoCliPorts => arduino_cli::ArduinoCliPorts.generate(ctx).await,
        ProviderKind::CargoWorkspaceMembers => {
            local_project::cargo_workspace::CargoWorkspaceMembers
                .generate(ctx)
                .await
        }
        ProviderKind::CargoTargets => cargo_metadata::CargoTargets.generate(ctx).await,
        ProviderKind::CargoFeatures => cargo_metadata::CargoFeatures.generate(ctx).await,
        ProviderKind::BrewCasksInstalled => brew::BrewCasksInstalled.generate(ctx).await,
        ProviderKind::BrewFormulaeInstalled => brew::BrewFormulaeInstalled.generate(ctx).await,
        ProviderKind::BrewFormulaeSearchable => brew::BrewFormulaeSearchable.generate(ctx).await,
        ProviderKind::DefaultsDomains => macos_defaults::DefaultsDomains.generate(ctx).await,
        ProviderKind::DockerContainers => docker::DockerContainers.generate(ctx).await,
        ProviderKind::DockerImages => docker::DockerImages.generate(ctx).await,
        ProviderKind::DockerNetworks => docker::DockerNetworks.generate(ctx).await,
        ProviderKind::DockerRunningContainers => {
            docker::DockerRunningContainers.generate(ctx).await
        }
        ProviderKind::DockerVolumes => docker::DockerVolumes.generate(ctx).await,
        ProviderKind::DsclGroups => dscl_principals::DsclGroups.generate(ctx).await,
        ProviderKind::DsclUsers => dscl_principals::DsclUsers.generate(ctx).await,
        ProviderKind::K8sContexts => kubectl::K8sContexts.generate(ctx).await,
        ProviderKind::K8sNamespaces => kubectl::K8sNamespaces.generate(ctx).await,
        ProviderKind::K8sNodes => kubectl::K8sNodes.generate(ctx).await,
        ProviderKind::K8sPods => kubectl::K8sPods.generate(ctx).await,
        ProviderKind::K8sResources => kubectl::K8sResources.generate(ctx).await,
        ProviderKind::K8sServices => kubectl::K8sServices.generate(ctx).await,
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
        ProviderKind::NpmAllDependencies => npm_local::NpmAllDependencies.generate(ctx).await,
        ProviderKind::NpmDependencies => npm_local::NpmDependencies.generate(ctx).await,
        ProviderKind::NpmDevDependencies => npm_local::NpmDevDependencies.generate(ctx).await,
        ProviderKind::NpmScripts => local_project::npm_scripts::NpmScripts.generate(ctx).await,
        ProviderKind::PandocInputFormats => pandoc::PandocInputFormats.generate(ctx).await,
        ProviderKind::PandocOutputFormats => pandoc::PandocOutputFormats.generate(ctx).await,
        ProviderKind::SystemdActiveUnits => systemd_units::SystemdActiveUnits.generate(ctx).await,
        ProviderKind::SystemdUnits => systemd_units::SystemdUnits.generate(ctx).await,
        ProviderKind::SystemdUserUnits => systemd_units::SystemdUserUnits.generate(ctx).await,
        ProviderKind::TmuxClients => tmux_state::TmuxClients.generate(ctx).await,
        ProviderKind::TmuxPanes => tmux_state::TmuxPanes.generate(ctx).await,
        ProviderKind::TmuxSessions => tmux_state::TmuxSessions.generate(ctx).await,
        ProviderKind::TmuxWindows => tmux_state::TmuxWindows.generate(ctx).await,
        #[cfg(test)]
        ProviderKind::TestEchoParams => Ok(ctx
            .params
            .iter()
            .map(|(key, value)| Suggestion {
                text: format!("{key}={value}"),
                ..Default::default()
            })
            .collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kind_from_type_str_unknown_returns_none() {
        // Exercises the unknown-provider path of the string→kind dispatcher. Any
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
        assert_eq!(kind_from_type_str("aws_sdk"), Some(ProviderKind::AwsSdk));
        assert_eq!(
            kind_from_type_str("aws_profile_names"),
            Some(ProviderKind::AwsProfileNames)
        );
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
            kind_from_type_str("cargo_targets"),
            Some(ProviderKind::CargoTargets)
        );
        assert_eq!(
            kind_from_type_str("cargo_features"),
            Some(ProviderKind::CargoFeatures)
        );
        assert_eq!(
            kind_from_type_str("brew_casks_installed"),
            Some(ProviderKind::BrewCasksInstalled)
        );
        assert_eq!(
            kind_from_type_str("brew_formulae_installed"),
            Some(ProviderKind::BrewFormulaeInstalled)
        );
        assert_eq!(
            kind_from_type_str("brew_formulae_searchable"),
            Some(ProviderKind::BrewFormulaeSearchable)
        );
        assert_eq!(
            kind_from_type_str("defaults_domains"),
            Some(ProviderKind::DefaultsDomains)
        );
        assert_eq!(
            kind_from_type_str("docker_containers"),
            Some(ProviderKind::DockerContainers)
        );
        assert_eq!(
            kind_from_type_str("docker_images"),
            Some(ProviderKind::DockerImages)
        );
        assert_eq!(
            kind_from_type_str("docker_networks"),
            Some(ProviderKind::DockerNetworks)
        );
        assert_eq!(
            kind_from_type_str("docker_running_containers"),
            Some(ProviderKind::DockerRunningContainers)
        );
        assert_eq!(
            kind_from_type_str("docker_volumes"),
            Some(ProviderKind::DockerVolumes)
        );
        assert_eq!(
            kind_from_type_str("dscl_groups"),
            Some(ProviderKind::DsclGroups)
        );
        assert_eq!(
            kind_from_type_str("dscl_users"),
            Some(ProviderKind::DsclUsers)
        );
        assert_eq!(
            kind_from_type_str("k8s_contexts"),
            Some(ProviderKind::K8sContexts)
        );
        assert_eq!(
            kind_from_type_str("k8s_namespaces"),
            Some(ProviderKind::K8sNamespaces)
        );
        assert_eq!(
            kind_from_type_str("k8s_nodes"),
            Some(ProviderKind::K8sNodes)
        );
        assert_eq!(kind_from_type_str("k8s_pods"), Some(ProviderKind::K8sPods));
        assert_eq!(
            kind_from_type_str("k8s_resources"),
            Some(ProviderKind::K8sResources)
        );
        assert_eq!(
            kind_from_type_str("k8s_services"),
            Some(ProviderKind::K8sServices)
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
            kind_from_type_str("npm_all_dependencies"),
            Some(ProviderKind::NpmAllDependencies)
        );
        assert_eq!(
            kind_from_type_str("npm_dependencies"),
            Some(ProviderKind::NpmDependencies)
        );
        assert_eq!(
            kind_from_type_str("npm_dev_dependencies"),
            Some(ProviderKind::NpmDevDependencies)
        );
        assert_eq!(
            kind_from_type_str("pandoc_input_formats"),
            Some(ProviderKind::PandocInputFormats)
        );
        assert_eq!(
            kind_from_type_str("pandoc_output_formats"),
            Some(ProviderKind::PandocOutputFormats)
        );
        assert_eq!(
            kind_from_type_str("systemd_active_units"),
            Some(ProviderKind::SystemdActiveUnits)
        );
        assert_eq!(
            kind_from_type_str("systemd_units"),
            Some(ProviderKind::SystemdUnits)
        );
        assert_eq!(
            kind_from_type_str("systemd_user_units"),
            Some(ProviderKind::SystemdUserUnits)
        );
        assert_eq!(
            kind_from_type_str("tmux_clients"),
            Some(ProviderKind::TmuxClients)
        );
        assert_eq!(
            kind_from_type_str("tmux_panes"),
            Some(ProviderKind::TmuxPanes)
        );
        assert_eq!(
            kind_from_type_str("tmux_sessions"),
            Some(ProviderKind::TmuxSessions)
        );
        assert_eq!(
            kind_from_type_str("tmux_windows"),
            Some(ProviderKind::TmuxWindows)
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
            params: Arc::new(BTreeMap::new()),
        };
        assert_eq!(ctx.cwd, PathBuf::from("/tmp"));
        assert!(ctx.env.is_empty());
        assert!(ctx.current_token.is_empty());
        assert!(ctx.params.is_empty());
    }

    #[test]
    fn test_provider_ctx_for_resolution_overlays_each_params_map() {
        let env = Arc::new(HashMap::from([(
            "SHELL".to_string(),
            "/bin/zsh".to_string(),
        )]));
        let base_params = Arc::new(BTreeMap::from([(
            "base".to_string(),
            "must-not-leak".to_string(),
        )]));
        let base = ProviderCtx {
            cwd: PathBuf::from("/tmp/project"),
            env: Arc::clone(&env),
            current_token: "tar".to_string(),
            params: base_params,
        };
        let first = ProviderResolution {
            kind: ProviderKind::NpmScripts,
            params: Arc::new(BTreeMap::from([(
                "package_manager".to_string(),
                "pnpm".to_string(),
            )])),
        };
        let second = ProviderResolution {
            kind: ProviderKind::CargoWorkspaceMembers,
            params: Arc::new(BTreeMap::from([(
                "workspace".to_string(),
                "members".to_string(),
            )])),
        };

        let first_ctx = base.for_resolution(&first);
        let second_ctx = base.for_resolution(&second);

        assert_eq!(first_ctx.cwd, base.cwd);
        assert_eq!(second_ctx.cwd, base.cwd);
        assert!(Arc::ptr_eq(&first_ctx.env, &env));
        assert!(Arc::ptr_eq(&second_ctx.env, &env));
        assert_eq!(first_ctx.current_token, "tar");
        assert_eq!(second_ctx.current_token, "tar");
        assert!(Arc::ptr_eq(&first_ctx.params, &first.params));
        assert!(Arc::ptr_eq(&second_ctx.params, &second.params));
        assert_eq!(first_ctx.params.as_ref(), first.params.as_ref());
        assert_eq!(second_ctx.params.as_ref(), second.params.as_ref());
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
    fn test_provider_ctx_env_returns_string_slice() {
        let ctx = ProviderCtx {
            cwd: PathBuf::from("/tmp"),
            env: Arc::new(HashMap::from([(
                "KUBECONFIG".to_string(),
                "/tmp/kubeconfig".to_string(),
            )])),
            current_token: String::new(),
            params: Arc::new(BTreeMap::new()),
        };

        assert_eq!(ctx.env("KUBECONFIG"), Some("/tmp/kubeconfig"));
        assert_eq!(ctx.env("MISSING"), None);
    }

    #[tokio::test]
    async fn test_resolve_rejects_relative_cwd_before_local_project_provider() {
        // A relative cwd would make local-project provider ancestor walks
        // consult the ghost-complete process cwd. The provider dispatch
        // boundary must reject it before any provider can read manifests
        // from the wrong project.
        assert!(
            std::env::current_dir()
                .unwrap()
                .join("Cargo.toml")
                .is_file(),
            "test requires the process cwd to contain a Cargo.toml"
        );
        let ctx =
            ProviderCtx::new_for_test(PathBuf::from("."), Arc::new(HashMap::new()), String::new());

        let results = resolve(ProviderKind::CargoWorkspaceMembers, &ctx)
            .await
            .unwrap();

        assert!(
            results.is_empty(),
            "relative cwd must not resolve providers from process cwd, got {results:?}"
        );
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
