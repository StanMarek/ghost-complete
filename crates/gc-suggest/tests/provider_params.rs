//! Integration tests for the new `ProviderCtx::params` plumbing —
//! the ux-9b precursor that lets future spec-driven providers
//! (`AwsSdk`, native-promoted Fig generators) read structured
//! parameters from their generator spec without trait-level changes.
//!
//! Three coverage points pinned by this file:
//!
//! 1. A stub `Provider` reads `ctx.params["foo"]` and returns the
//!    expected value — proves the channel works end-to-end.
//! 2. `ProviderCtx::params_hash()` is stable across calls with the
//!    same params, and changes when params change — proves the hash
//!    is suitable as a cache key.
//! 3. `ProviderCtx::new(...)` initializes `params` to an empty map —
//!    proves the new field is purely additive for existing call sites.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use gc_suggest::providers::{Provider, ProviderCtx};
use gc_suggest::types::{Suggestion, SuggestionKind};

/// Stub provider that reads `ctx.params["foo"]` and surfaces it as a
/// single `Suggestion`. Mirrors the shape of a future spec-driven
/// provider that consumes `params` (e.g., AWS service selection).
struct ParamReader;

impl Provider for ParamReader {
    fn name(&self) -> &'static str {
        "param_reader_stub"
    }

    async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
        let text = ctx
            .params
            .get("foo")
            .cloned()
            .unwrap_or_else(|| String::from("<missing>"));
        Ok(vec![Suggestion {
            text,
            kind: SuggestionKind::ProviderValue,
            ..Default::default()
        }])
    }
}

#[tokio::test]
async fn stub_provider_reads_params_foo() {
    let mut params = BTreeMap::new();
    params.insert("foo".to_string(), "bar".to_string());
    let ctx = ProviderCtx {
        cwd: PathBuf::from("/tmp"),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(params),
    };
    let out = ParamReader.generate(&ctx).await.unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].text, "bar");
}

#[tokio::test]
async fn stub_provider_sees_missing_when_params_absent() {
    let ctx = ProviderCtx {
        cwd: PathBuf::from("/tmp"),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(BTreeMap::new()),
    };
    let out = ParamReader.generate(&ctx).await.unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].text, "<missing>");
}

#[test]
fn params_hash_is_stable_for_equal_params() {
    let mut a = BTreeMap::new();
    a.insert("foo".to_string(), "bar".to_string());
    a.insert("baz".to_string(), "qux".to_string());

    let mut b = BTreeMap::new();
    // Reverse insertion order to verify BTreeMap iteration determinism.
    b.insert("baz".to_string(), "qux".to_string());
    b.insert("foo".to_string(), "bar".to_string());

    let ctx_a = ProviderCtx {
        cwd: PathBuf::from("/tmp"),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(a),
    };
    let ctx_b = ProviderCtx {
        cwd: PathBuf::from("/tmp"),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(b),
    };

    assert_eq!(ctx_a.params_hash(), ctx_b.params_hash());
}

#[test]
fn params_hash_differs_for_different_params() {
    let mut a = BTreeMap::new();
    a.insert("foo".to_string(), "bar".to_string());

    let mut b = BTreeMap::new();
    b.insert("foo".to_string(), "qux".to_string());

    let ctx_a = ProviderCtx {
        cwd: PathBuf::from("/tmp"),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(a),
    };
    let ctx_b = ProviderCtx {
        cwd: PathBuf::from("/tmp"),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(b),
    };

    assert_ne!(ctx_a.params_hash(), ctx_b.params_hash());
}

#[test]
fn params_hash_empty_is_stable_constant() {
    // Two independently-constructed empty-params ctxs hash identically.
    // Keeps the cache-key contract obvious for the common case.
    let ctx_a = ProviderCtx {
        cwd: PathBuf::from("/tmp"),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(BTreeMap::new()),
    };
    let ctx_b = ProviderCtx {
        cwd: PathBuf::from("/tmp"),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(BTreeMap::new()),
    };
    assert_eq!(ctx_a.params_hash(), ctx_b.params_hash());
}

#[test]
fn new_initializes_params_to_empty_map() {
    // The 3-arg ::new constructor MUST default `params` to an empty
    // map so existing call sites (engine, gc-pty handler, every
    // existing provider test) compile unchanged.
    let ctx = ProviderCtx::new(
        PathBuf::from("/tmp"),
        Arc::new(HashMap::new()),
        "tok".to_string(),
    )
    .expect("absolute cwd accepted");
    assert!(ctx.params.is_empty(), "default params must be empty");
}

#[test]
fn params_are_visible_through_btreemap_api() {
    // BTreeMap exposes Ord-keyed iteration for callers that want to
    // build a deterministic representation of the params (e.g.
    // generator-cache disk keys). This test pins that ordering so a
    // future swap to HashMap would fail loudly.
    let mut params = BTreeMap::new();
    params.insert("z_last".to_string(), "3".to_string());
    params.insert("a_first".to_string(), "1".to_string());
    params.insert("m_middle".to_string(), "2".to_string());

    let ctx = ProviderCtx {
        cwd: PathBuf::from("/tmp"),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(params),
    };

    let keys: Vec<&str> = ctx.params.keys().map(|s| s.as_str()).collect();
    assert_eq!(keys, vec!["a_first", "m_middle", "z_last"]);
}
