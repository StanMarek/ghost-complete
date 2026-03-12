#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionKind {
    Command,
    Subcommand,
    Flag,
    FilePath,
    Directory,
    GitBranch,
    GitTag,
    GitRemote,
    History,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionSource {
    Filesystem,
    Git,
    History,
    Commands,
    Spec,
    Script,
}

#[derive(Debug, Clone)]
pub struct Suggestion {
    pub text: String,
    pub description: Option<String>,
    pub kind: SuggestionKind,
    pub source: SuggestionSource,
    pub score: u32,
    pub match_indices: Vec<u32>,
}

impl Default for Suggestion {
    fn default() -> Self {
        Self {
            text: String::new(),
            description: None,
            kind: SuggestionKind::Command,
            source: SuggestionSource::Commands,
            score: 0,
            match_indices: Vec::new(),
        }
    }
}
