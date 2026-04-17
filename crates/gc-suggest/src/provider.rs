use std::path::Path;

use anyhow::Result;
use gc_buffer::CommandContext;

use crate::types::Suggestion;

pub trait Provider: Send + Sync {
    fn provide(&self, ctx: &CommandContext, cwd: &Path) -> Result<Vec<Suggestion>>;
}
