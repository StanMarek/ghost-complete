//! Suggestion engine with multiple providers and fuzzy ranking.
//!
//! Dispatches to providers (filesystem, git, history, `$PATH` commands,
//! Fig-compatible JSON specs) and fuzzy-ranks results with `nucleo`.

pub mod alias;
pub mod cache;
pub mod commands;
pub mod embedded;
mod engine;
mod env;
mod filesystem;
pub mod frecency;
pub mod fuzzy;
pub mod git;
pub mod history;
pub mod json_path;
pub mod pipeline;
pub mod priority;
mod provider;
pub mod providers;
pub mod script;
pub mod spec_dirs;
pub mod specs;
pub mod ssh;
pub mod transform;
pub mod types;

pub use embedded::EMBEDDED_SPECS;
pub use engine::{SuggestionEngine, SyncResult};
pub use json_path::{JsonPath, JsonPathSegment};
pub use pipeline::try_run_pipeline;
pub use specs::{
    check_json_depth, parse_spec_checked_and_sanitized, sanitize_spec_strings, CompletionSpec,
    SpecLoadResult, SpecStore, MAX_SPEC_JSON_DEPTH,
};
pub use types::{Suggestion, SuggestionKind, SuggestionSource};
