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
    EnvVar,
}

impl SuggestionKind {
    /// Display priority for popup ordering (lower = shown first).
    /// Branches/tags before subcommands/flags, flags before filesystem.
    /// Short tag used as a component in frecency keys so that different kinds
    /// under the same command don't share a score bucket.
    pub fn key_tag(self) -> &'static str {
        match self {
            Self::Command => "cmd",
            Self::Subcommand => "sub",
            Self::Flag => "flag",
            Self::FilePath => "file",
            Self::Directory => "dir",
            Self::GitBranch => "branch",
            Self::GitTag => "tag",
            Self::GitRemote => "remote",
            Self::History => "hist",
            Self::EnvVar => "env",
        }
    }

    /// Display priority for popup ordering (lower = shown first).
    /// Branches/tags before subcommands/flags, flags before filesystem.
    pub fn sort_priority(self) -> u8 {
        match self {
            Self::GitBranch => 0,
            Self::GitTag => 1,
            Self::GitRemote => 2,
            Self::Subcommand => 3,
            Self::Flag => 4,
            // EnvVar and Command intentionally share priority 5 — they are
            // peers in the hierarchy (both are "things the user can invoke or
            // reference at arg position"), and neither should outrank the
            // other by kind alone. Downstream in `rank_with_history`, the
            // sort chain is: history-bucket → fuzzy score (desc) → this
            // priority → text (alphabetic). By the time this tie is reached,
            // fuzzy scores are already equal, so the final order falls out
            // alphabetically by text — which is the intended behavior.
            // Picking different numbers here is a behavior change, not a
            // code-quality fix.
            Self::EnvVar => 5,
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
    Env,
    SshConfig,
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
