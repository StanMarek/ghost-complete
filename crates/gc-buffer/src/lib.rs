//! Command line buffer reconstruction and context detection.
//!
//! Tokenizes the raw command line and determines the current command, argument
//! position, pipe/redirect state, and quoting context for the suggestion engine.

mod context;
mod tokenizer;

pub use context::{parse_command_context, CommandContext};
pub use tokenizer::{tokenize, QuoteState, Token, TokenizeResult};
