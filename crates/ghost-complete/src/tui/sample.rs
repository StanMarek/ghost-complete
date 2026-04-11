use gc_suggest::{Suggestion, SuggestionKind, SuggestionSource};

pub fn sample_suggestions() -> Vec<Suggestion> {
    vec![
        Suggestion {
            text: "checkout".to_string(),
            description: Some("Switch branches or restore files".to_string()),
            kind: SuggestionKind::Subcommand,
            source: SuggestionSource::Spec,
            score: 100,
            match_indices: vec![0, 1, 2], // "che" highlighted
        },
        Suggestion {
            text: "commit".to_string(),
            description: Some("Record changes to the repository".to_string()),
            kind: SuggestionKind::Subcommand,
            source: SuggestionSource::Spec,
            score: 95,
            match_indices: vec![0, 1],
        },
        Suggestion {
            text: "--force".to_string(),
            description: Some("Force the operation".to_string()),
            kind: SuggestionKind::Flag,
            source: SuggestionSource::Spec,
            score: 80,
            match_indices: vec![],
        },
        Suggestion {
            text: "src/main.rs".to_string(),
            description: None,
            kind: SuggestionKind::FilePath,
            source: SuggestionSource::Filesystem,
            score: 70,
            match_indices: vec![4, 5, 6, 7],
        },
        Suggestion {
            text: "target/".to_string(),
            description: None,
            kind: SuggestionKind::Directory,
            source: SuggestionSource::Filesystem,
            score: 60,
            match_indices: vec![],
        },
        Suggestion {
            text: "feature/config-tui".to_string(),
            description: None,
            kind: SuggestionKind::GitBranch,
            source: SuggestionSource::Git,
            score: 50,
            match_indices: vec![8, 9, 10, 11, 12, 13],
        },
        Suggestion {
            text: "cargo build --release".to_string(),
            description: Some("from history".to_string()),
            kind: SuggestionKind::History,
            source: SuggestionSource::History,
            score: 40,
            match_indices: vec![],
        },
        Suggestion {
            text: "push".to_string(),
            description: Some("Update remote refs".to_string()),
            kind: SuggestionKind::Subcommand,
            source: SuggestionSource::Spec,
            score: 90,
            match_indices: vec![],
        },
    ]
}
