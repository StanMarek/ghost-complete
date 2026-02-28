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
}

#[derive(Debug, Clone)]
pub struct Suggestion {
    pub text: String,
    pub description: Option<String>,
    pub kind: SuggestionKind,
    pub source: SuggestionSource,
    pub score: u32,
}
