//! Suggestion engine with multiple providers and fuzzy ranking.
//!
//! Dispatches to providers (filesystem, git, history, `$PATH` commands,
//! Fig-compatible JSON specs) and fuzzy-ranks results with `nucleo`.

pub mod alias;
pub mod cache;
pub mod commands;
mod engine;
mod env;
mod filesystem;
pub mod frecency;
pub mod fuzzy;
pub mod git;
pub mod history;
mod provider;
pub mod script;
pub mod spec_dirs;
pub mod specs;
pub mod ssh;
pub mod transform;
pub mod types;

pub use engine::{SuggestionEngine, SyncResult};
pub use specs::{CompletionSpec, SpecLoadResult, SpecStore};
pub use types::{Suggestion, SuggestionKind, SuggestionSource};
