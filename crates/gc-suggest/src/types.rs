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

impl SuggestionKind {
    /// Display priority for popup ordering (lower = shown first).
    /// Branches/tags before subcommands/flags, flags before filesystem.
    pub fn sort_priority(self) -> u8 {
        match self {
            Self::GitBranch => 0,
            Self::GitTag => 1,
            Self::GitRemote => 2,
            Self::Subcommand => 3,
            Self::Flag => 4,
            Self::Command => 5,
            Self::Directory => 6,
            Self::FilePath => 7,
            Self::History => 8,
        }
    }
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
