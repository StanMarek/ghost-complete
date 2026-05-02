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
                DynamicResult::Error { provider, message } => {
                    tracing::warn!(provider = %provider, "dynamic provider failed: {message}");
                    aggregation.failed.push(provider.to_string())
                }
            }
        }
        aggregation
    }

    pub fn terminal_from_aggregation(aggregation: &DynamicAggregation, now: Instant) -> Self {
        Self::terminal_for_outcome(
            !aggregation.loaded.is_empty(),
            &aggregation.failed,
            aggregation.empty_count,
            now,
        )
    }

    /// Borrow-only variant of [`Self::terminal_from_aggregation`] — avoids cloning the loaded vec.
    pub fn terminal_for_outcome(
        loaded_non_empty: bool,
        failed: &[String],
        empty_count: usize,
        now: Instant,
    ) -> Self {
        if loaded_non_empty && !failed.is_empty() {
            Self::PartialError {
                failed: failed.to_vec(),
                since: now,
            }
        } else if !failed.is_empty() {
            Self::Error {
                failed: failed.to_vec(),
                since: now,
            }
        } else if !loaded_non_empty && empty_count > 0 {
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
    use crate::dynamic_result::ProviderTag;
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
                message: "boom".into(),
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

    #[test]
    fn terminal_feedback_failed_only_is_error() {
        let now = Instant::now();
        let aggregation = DynamicAggregation {
            loaded: Vec::new(),
            empty_count: 0,
            failed: vec!["git branches".into()],
        };
        assert!(matches!(
            AsyncFeedback::terminal_from_aggregation(&aggregation, now),
            AsyncFeedback::Error { .. }
        ));
    }

    #[test]
    fn terminal_feedback_empty_only_is_empty() {
        let now = Instant::now();
        let aggregation = DynamicAggregation {
            loaded: Vec::new(),
            empty_count: 1,
            failed: Vec::new(),
        };
        assert!(matches!(
            AsyncFeedback::terminal_from_aggregation(&aggregation, now),
            AsyncFeedback::Empty { .. }
        ));
    }

    #[test]
    fn terminal_feedback_all_loaded_is_idle() {
        let now = Instant::now();
        let aggregation = DynamicAggregation {
            loaded: vec![suggestion("main")],
            empty_count: 0,
            failed: Vec::new(),
        };
        assert!(matches!(
            AsyncFeedback::terminal_from_aggregation(&aggregation, now),
            AsyncFeedback::Idle
        ));
    }
}
