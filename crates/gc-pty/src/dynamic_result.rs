use std::fmt;

use gc_suggest::{git::GitQueryKind, providers::ProviderKind, Suggestion};

#[derive(Debug, Clone)]
pub enum DynamicResult {
    Loaded {
        provider: ProviderTag,
        suggestions: Vec<Suggestion>,
    },
    Empty {
        provider: ProviderTag,
    },
    Error {
        provider: ProviderTag,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderTag {
    Script(String),
    Git(GitQueryKind),
    Provider(ProviderKind),
}

impl fmt::Display for ProviderTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Script(command) if command.is_empty() => f.write_str("script"),
            Self::Script(command) => write!(f, "{command} script"),
            Self::Git(kind) => write!(f, "git {}", git_kind_name(*kind)),
            Self::Provider(kind) => f.write_str(kind.type_str()),
        }
    }
}

fn git_kind_name(kind: GitQueryKind) -> &'static str {
    match kind {
        GitQueryKind::Branches => "branches",
        GitQueryKind::Tags => "tags",
        GitQueryKind::Remotes => "remotes",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gc_suggest::SuggestionKind;

    #[test]
    fn provider_tag_display_is_stable() {
        assert_eq!(ProviderTag::Script("git".into()).to_string(), "git script");
        assert_eq!(
            ProviderTag::Git(GitQueryKind::Branches).to_string(),
            "git branches"
        );
        assert_eq!(
            ProviderTag::Provider(ProviderKind::NpmScripts).to_string(),
            "npm_scripts"
        );
    }

    #[test]
    fn dynamic_result_variants_carry_payloads() {
        let suggestion = Suggestion {
            text: "main".into(),
            kind: SuggestionKind::GitBranch,
            ..Default::default()
        };
        let result = DynamicResult::Loaded {
            provider: ProviderTag::Git(GitQueryKind::Branches),
            suggestions: vec![suggestion],
        };
        match result {
            DynamicResult::Loaded { suggestions, .. } => assert_eq!(suggestions.len(), 1),
            _ => panic!("expected loaded"),
        }
    }
}
