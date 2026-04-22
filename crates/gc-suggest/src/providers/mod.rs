//! Phase 3A native providers — async, context-aware suggestion sources
//! that replace JavaScript-backed Fig generators for a curated set of
//! commands.
//!
//! This module is the scaffolding counterpart to `crate::git`:
//! - `Provider` is the async trait every native provider implements.
//! - `ProviderCtx` is the context handed to each `generate` call (cwd,
//!   environment, current token).
//! - `ProviderKind` is a closed enum listing every registered provider.
//!   Concrete variants are added by T2–T9; the enum is intentionally
//!   empty at T1 so the scaffolding lands ahead of any implementations.
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
//! stable Rust, and the Phase 3A plan forbids adding the `async-trait`
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

/// Context passed to every provider's `generate` call. Owned by the
/// engine; providers receive it by reference so the shared env map is
/// not cloned per invocation.
pub struct ProviderCtx {
    /// Working directory the shell was in when the completion trigger
    /// fired. Providers that shell out to external tools pass this as
    /// the subprocess cwd.
    pub cwd: PathBuf,
    /// Snapshot of the shell's environment at trigger time. `Arc`
    /// because the engine hands the same map to every provider in a
    /// single resolution pass.
    pub env: Arc<HashMap<String, String>>,
    /// The partially-typed token the user is currently completing. May
    /// be empty when the trigger fires on a space after a subcommand.
    pub current_token: String,
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

/// Registered native providers. Variants are added by T2–T9; empty at
/// T1 is intentional. An empty enum compiles and `match kind {}` is a
/// sound exhaustive match with zero arms, so every downstream site
/// that dispatches on `ProviderKind` remains well-typed without a
/// placeholder variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {}

/// Map a spec's `"type"` string to a `ProviderKind`, or `None` if the
/// string does not name a registered native provider.
///
/// This is the single source of truth wired into
/// `specs::collect_generators`: when a `GeneratorSpec.generator_type`
/// returns `Some(kind)` here, the spec resolution routes it into
/// `provider_generators` instead of the script path. New providers
/// (T2–T9) add one arm each.
pub fn kind_from_type_str(type_str: &str) -> Option<ProviderKind> {
    // Catchall-only at T1: no providers are registered yet. Each future
    // provider task adds a single `"<name>" => Some(ProviderKind::<Variant>),`
    // arm above the catchall and removes this allow. Written as a match
    // (rather than an immediate `None`) to preserve the shape future
    // tasks extend, not to gesture at a real dispatch today.
    #[allow(clippy::match_single_binding)]
    match type_str {
        _ => None,
    }
}

/// Dispatch a single provider kind against `ctx`. The engine iterates
/// the slice of kinds from a `SpecResolution` and awaits each.
///
/// At T1 `ProviderKind` has no variants, so the body is the unreachable
/// empty match. Each concrete provider task turns this into a real
/// dispatch arm: instantiate the provider (cheaply — these are stateless
/// structs) and call `generate(ctx).await`.
pub async fn resolve(kind: ProviderKind, _ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
    match kind {}
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
    fn test_provider_ctx_is_constructible() {
        // Sanity: ProviderCtx fields are public and the struct is usable
        // from downstream call sites (engine + tests in T2–T9). This is
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
}
