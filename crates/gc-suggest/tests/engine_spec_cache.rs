//! Smoke tests for `SuggestionEngine::spawn_spec_cache_sweep` wiring.
//!
//! The engine method is a thin passthrough to
//! [`gc_suggest::specs::spawn_spec_cache_sweep`]; these tests guard the
//! `Arc<SpecStore>` clone and the `enabled()` short-circuit at the engine
//! layer.

use std::path::PathBuf;

use gc_config::SpecCacheConfig;
use gc_suggest::SuggestionEngine;

fn engine() -> SuggestionEngine {
    let spec_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../specs");
    SuggestionEngine::new(&[spec_dir]).expect("engine must construct from workspace specs")
}

#[tokio::test(flavor = "current_thread")]
async fn engine_spawn_spec_cache_sweep_returns_none_when_disabled() {
    let engine = engine();
    let cfg = SpecCacheConfig::default(); // idle_ttl_secs = 0 → disabled
    assert!(
        engine.spawn_spec_cache_sweep(cfg).is_none(),
        "engine wiring must honour the eviction-disabled short-circuit"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn engine_spawn_spec_cache_sweep_returns_some_when_enabled() {
    let engine = engine();
    let cfg = SpecCacheConfig {
        idle_ttl_secs: 1,
        sweep_interval_secs: 1,
        keep_warm: vec![],
        max_resident_mb: 0,
    };
    let sweep = engine.spawn_spec_cache_sweep(cfg);
    assert!(
        sweep.is_some(),
        "engine wiring must spawn a sweep guard when eviction is enabled"
    );
}
