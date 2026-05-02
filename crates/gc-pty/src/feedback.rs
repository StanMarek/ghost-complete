use std::time::Instant;

use gc_suggest::Suggestion;

use crate::dynamic_result::DynamicResult;

#[derive(Debug, Clone, Default)]
pub enum AsyncFeedback {
    #[default]
    Idle,
    Loading {
        spawned_at: Instant,
    },
    Empty {
        since: Instant,
    },
    Error {
        failed: Vec<String>,
        since: Instant,
    },
    PartialError {
        failed: Vec<String>,
        since: Instant,
    },
}

#[derive(Debug, Default)]
pub struct DynamicAggregation {
    pub loaded: Vec<Suggestion>,
    pub empty_count: usize,
    pub failed: Vec<String>,
}

impl AsyncFeedback {
    pub fn aggregate(results: Vec<DynamicResult>) -> DynamicAggregation {
        let mut aggregation = DynamicAggregation::default();
        for result in results {
            match result {
                DynamicResult::Loaded { suggestions, .. } => {
                    if suggestions.is_empty() {
                        aggregation.empty_count += 1;
                    } else {
                        aggregation.loaded.extend(suggestions);
                    }
                }
                DynamicResult::Empty { .. } => aggregation.empty_count += 1,
                DynamicResult::Error { provider, .. } => {
                    aggregation.failed.push(provider.to_string())
                }
            }
        }
        aggregation
    }

    pub fn terminal_from_aggregation(aggregation: &DynamicAggregation, now: Instant) -> Self {
        if !aggregation.loaded.is_empty() && !aggregation.failed.is_empty() {
            Self::PartialError {
                failed: aggregation.failed.clone(),
                since: now,
            }
        } else if !aggregation.failed.is_empty() {
            Self::Error {
                failed: aggregation.failed.clone(),
                since: now,
            }
        } else if aggregation.loaded.is_empty() && aggregation.empty_count > 0 {
            Self::Empty { since: now }
        } else {
            Self::Idle
        }
    }

    pub fn is_loading(&self) -> bool {
        matches!(self, Self::Loading { .. })
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Empty { .. } | Self::Error { .. } | Self::PartialError { .. }
        )
    }

    pub fn since(&self) -> Option<Instant> {
        match self {
            Self::Empty { since }
            | Self::Error { since, .. }
            | Self::PartialError { since, .. } => Some(*since),
            Self::Idle | Self::Loading { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynamic_result::{ErrorKind, ProviderTag};
    use gc_suggest::{git::GitQueryKind, SuggestionKind};

    fn suggestion(text: &str) -> Suggestion {
        Suggestion {
            text: text.into(),
            kind: SuggestionKind::GitBranch,
            ..Default::default()
        }
    }

    #[test]
    fn aggregate_loaded_empty_and_error() {
        let aggregation = AsyncFeedback::aggregate(vec![
            DynamicResult::Loaded {
                provider: ProviderTag::Git(GitQueryKind::Branches),
                suggestions: vec![suggestion("main")],
            },
            DynamicResult::Empty {
                provider: ProviderTag::Git(GitQueryKind::Tags),
            },
            DynamicResult::Error {
                provider: ProviderTag::Script("git".into()),
                kind: ErrorKind::Runtime("boom".into()),
            },
        ]);
        assert_eq!(aggregation.loaded.len(), 1);
        assert_eq!(aggregation.empty_count, 1);
        assert_eq!(aggregation.failed, vec!["git script"]);
    }

    #[test]
    fn terminal_feedback_prefers_partial_error_with_loaded_results() {
        let now = Instant::now();
        let aggregation = DynamicAggregation {
            loaded: vec![suggestion("main")],
            empty_count: 0,
            failed: vec!["git branches".into()],
        };
        assert!(matches!(
            AsyncFeedback::terminal_from_aggregation(&aggregation, now),
            AsyncFeedback::PartialError { .. }
        ));
    }
}
