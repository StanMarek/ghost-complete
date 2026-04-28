use crate::priority::Priority;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
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
    /// Dynamic, spec-driven argument value produced by a native provider
    /// (e.g. arduino-cli FQBNs, pandoc format names, conda env names,
    /// multipass VM names, macOS `defaults` domains). Grouped with other
    /// arg-position values for sort order, and given its own frecency
    /// bucket so accepting one provider's value does not boost unrelated
    /// values with the same text from a different provider.
    ProviderValue,
    /// Static enum-style value declared in `args.suggestions` inside a spec.
    /// Sits between `Subcommand`/`ProviderValue` (70) and `EnvVar` (50) so enum
    /// values surface above environment variables, generic $PATH commands, and
    /// flags but below subcommands and dynamic provider results.
    EnumValue,
}

impl SuggestionKind {
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
            Self::ProviderValue => "provider",
            Self::EnumValue => "enum",
        }
    }

    /// Base priority for this `SuggestionKind` when the suggestion does
    /// not declare its own. Numbers chosen so that branches > generator
    /// output > flags > filesystem, with comfortable headroom for spec
    /// overrides.
    pub fn base_priority(self) -> Priority {
        Priority::new(match self {
            Self::GitBranch => 80,
            Self::GitTag => 75,
            Self::GitRemote => 70,
            Self::Subcommand => 70,
            Self::ProviderValue => 70,
            Self::EnvVar => 50,
            Self::Command => 40,
            Self::EnumValue => 65,
            Self::Flag => 30,
            Self::Directory => 25,
            Self::FilePath => 20,
            Self::History => 10,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum SuggestionSource {
    Filesystem,
    Git,
    History,
    Commands,
    Spec,
    Script,
    Env,
    SshConfig,
    /// Native providers (e.g. `arduino_cli_boards`). Distinct from
    /// `Spec`/`Script` so providers are identifiable in telemetry and
    /// downstream filtering without overlapping the legacy paths.
    Provider,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Suggestion {
    pub text: String,
    pub description: Option<String>,
    pub kind: SuggestionKind,
    pub source: SuggestionSource,
    pub score: u32,
    pub match_indices: Vec<u32>,
    /// Spec-declared rank hint, range 0..=100. When `None`, falls back to
    /// the kind's base priority (see `SuggestionKind::base_priority`).
    /// Higher values rank earlier in the popup.
    pub priority: Option<Priority>,
}

impl Default for Suggestion {
    fn default() -> Self {
        // Neutral default: `ProviderValue` + `Provider` is a kind/source
        // pair with no legacy overlap, so the default does not pretend to
        // be a shell command or a Commands-source entry. Every production
        // call site that builds a `Suggestion` via `..Default::default()`
        // sets `kind` and `source` explicitly; this default is only
        // observable when a caller forgets to, and picking a neutral
        // "dynamic arg-value" bucket is strictly better than defaulting
        // to Command (which would misclassify silently).
        Self {
            text: String::new(),
            description: None,
            kind: SuggestionKind::ProviderValue,
            source: SuggestionSource::Provider,
            score: 0,
            match_indices: Vec::new(),
            priority: None,
        }
    }
}

#[cfg(test)]
mod kind_invariants {
    use super::*;

    // Pin the behavioral contracts for `ProviderValue` + the neutral
    // `Suggestion::default()`. Silent drift in any of these values would
    // cross-pollute frecency buckets (key_tag) or mis-rank the popup
    // (base priority) without being caught by the relative-ordering tests
    // in `engine.rs`.
    #[test]
    fn provider_value_contract() {
        assert_eq!(SuggestionKind::ProviderValue.key_tag(), "provider");
        assert_eq!(SuggestionKind::ProviderValue.base_priority().get(), 70);
        assert_eq!(Suggestion::default().kind, SuggestionKind::ProviderValue);
        assert_eq!(Suggestion::default().source, SuggestionSource::Provider);
    }

    #[test]
    fn suggestion_priority_defaults_to_none() {
        let s = Suggestion::default();
        assert_eq!(s.priority, None);
    }

    #[test]
    fn enum_value_contract() {
        assert_eq!(SuggestionKind::EnumValue.key_tag(), "enum");
        assert_eq!(SuggestionKind::EnumValue.base_priority().get(), 65);
    }
}
