//! Suggestion engine with multiple providers and fuzzy ranking.
//!
//! Dispatches to providers (filesystem, git, history, `$PATH` commands,
//! Fig-compatible JSON specs) and fuzzy-ranks results with `nucleo`.

mod commands;
mod engine;
mod filesystem;
mod fuzzy;
mod git;
mod history;
mod provider;
pub mod specs;
pub mod types;

pub use engine::SuggestionEngine;
pub use specs::{CompletionSpec, SpecLoadResult, SpecStore};
pub use types::{Suggestion, SuggestionKind, SuggestionSource};
