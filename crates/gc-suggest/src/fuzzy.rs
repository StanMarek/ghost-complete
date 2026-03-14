use nucleo::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32String};

use crate::types::{Suggestion, SuggestionSource};

pub const DEFAULT_MAX_RESULTS: usize = 50;

pub fn rank(query: &str, mut suggestions: Vec<Suggestion>, max_results: usize) -> Vec<Suggestion> {
    if query.is_empty() {
        // Sort by kind priority (branches before flags before files, etc.)
        suggestions.sort_by(|a, b| {
            a.kind
                .sort_priority()
                .cmp(&b.kind.sort_priority())
                .then_with(|| a.text.cmp(&b.text))
        });
        suggestions.truncate(max_results);
        return suggestions;
    }

    let pattern = Pattern::new(
        query,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut indices_buf = Vec::new();

    suggestions.retain_mut(|s| {
        let haystack = Utf32String::from(s.text.as_str());
        indices_buf.clear();
        match pattern.indices(haystack.slice(..), &mut matcher, &mut indices_buf) {
            Some(score) => {
                s.score = score;
                indices_buf.sort_unstable();
                indices_buf.dedup();
                s.match_indices = indices_buf.clone();
                true
            }
            None => false,
        }
    });

    suggestions.sort_by(|a, b| {
        let a_hist = a.source == SuggestionSource::History;
        let b_hist = b.source == SuggestionSource::History;
        a_hist
            .cmp(&b_hist)
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.kind.sort_priority().cmp(&b.kind.sort_priority()))
            .then_with(|| a.text.cmp(&b.text))
    });
    suggestions.truncate(max_results);
    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SuggestionSource;

    fn make(text: &str) -> Suggestion {
        Suggestion {
            text: text.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_empty_query_returns_all() {
        let items: Vec<Suggestion> = (0..10).map(|i| make(&format!("item{i}"))).collect();
        let result = rank("", items, DEFAULT_MAX_RESULTS);
        assert_eq!(result.len(), 10);
    }

    #[test]
    fn test_fuzzy_match_filters() {
        let items = vec![make("checkout"), make("cherry-pick"), make("zzzzz")];
        let result = rank("che", items, DEFAULT_MAX_RESULTS);
        assert!(result.iter().any(|s| s.text == "checkout"));
        assert!(result.iter().any(|s| s.text == "cherry-pick"));
        assert!(!result.iter().any(|s| s.text == "zzzzz"));
    }

    #[test]
    fn test_exact_prefix_scores_higher() {
        let items = vec![make("achievement"), make("checkout")];
        let result = rank("check", items, DEFAULT_MAX_RESULTS);
        assert!(!result.is_empty());
        assert_eq!(result[0].text, "checkout");
    }

    #[test]
    fn test_no_matches_returns_empty() {
        let items = vec![make("alpha"), make("beta"), make("gamma")];
        let result = rank("zzzzxxx", items, DEFAULT_MAX_RESULTS);
        assert!(result.is_empty());
    }

    #[test]
    fn test_max_results_cap() {
        let items: Vec<Suggestion> = (0..100).map(|i| make(&format!("item{i}"))).collect();
        let result = rank("item", items, DEFAULT_MAX_RESULTS);
        assert!(result.len() <= DEFAULT_MAX_RESULTS);
    }

    #[test]
    fn test_custom_max_results() {
        let items: Vec<Suggestion> = (0..100).map(|i| make(&format!("item{i}"))).collect();
        let result = rank("item", items, 5);
        assert!(result.len() <= 5);
    }

    #[test]
    fn test_history_items_sorted_after_non_history() {
        let items = vec![
            Suggestion {
                text: "checkout".to_string(),
                source: SuggestionSource::History,
                ..Default::default()
            },
            Suggestion {
                text: "cherry-pick".to_string(),
                source: SuggestionSource::Commands,
                ..Default::default()
            },
            Suggestion {
                text: "check".to_string(),
                source: SuggestionSource::History,
                ..Default::default()
            },
            Suggestion {
                text: "chmod".to_string(),
                source: SuggestionSource::Commands,
                ..Default::default()
            },
        ];
        let result = rank("ch", items, DEFAULT_MAX_RESULTS);
        // All non-history items should come before any history item
        let first_hist = result
            .iter()
            .position(|s| s.source == SuggestionSource::History);
        let last_non_hist = result
            .iter()
            .rposition(|s| s.source != SuggestionSource::History);
        if let (Some(fh), Some(lnh)) = (first_hist, last_non_hist) {
            assert!(
                lnh < fh,
                "non-history items should all precede history items: {result:?}"
            );
        }
    }

    #[test]
    fn test_scores_are_set() {
        let items = vec![make("checkout"), make("cherry-pick")];
        let result = rank("ch", items, DEFAULT_MAX_RESULTS);
        for s in &result {
            assert!(s.score > 0, "score should be > 0 after ranking");
        }
    }

    #[test]
    fn test_match_indices_populated() {
        let items = vec![make("checkout"), make("cherry-pick")];
        let result = rank("che", items, DEFAULT_MAX_RESULTS);
        for s in &result {
            assert!(
                !s.match_indices.is_empty(),
                "match_indices should be populated for '{}'",
                s.text
            );
        }
        let checkout = result.iter().find(|s| s.text == "checkout").unwrap();
        assert_eq!(checkout.match_indices, vec![0, 1, 2]);
    }

    #[test]
    fn test_match_indices_sorted_and_deduped() {
        let items = vec![make("abcabc")];
        let result = rank("abc", items, DEFAULT_MAX_RESULTS);
        let s = &result[0];
        for window in s.match_indices.windows(2) {
            assert!(window[0] < window[1], "indices must be sorted and unique");
        }
    }

    #[test]
    fn test_empty_query_no_match_indices() {
        let items = vec![make("checkout")];
        let result = rank("", items, DEFAULT_MAX_RESULTS);
        assert!(result[0].match_indices.is_empty());
    }
}
