use nucleo::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32String};

use crate::priority;
use crate::types::{Suggestion, SuggestionSource};

pub const DEFAULT_MAX_RESULTS: usize = 50;

pub fn rank(query: &str, mut suggestions: Vec<Suggestion>, max_results: usize) -> Vec<Suggestion> {
    if query.is_empty() {
        // Empty query: priority alone determines order (score is 0 for all).
        suggestions.sort_by(|a, b| {
            priority::effective(b)
                .cmp(&priority::effective(a))
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

    // History is partitioned to the bottom regardless of fuzzy score so a
    // boosted history match can never outrank domain content. The two
    // merge paths in `gc-pty/src/handler.rs` that bypass `rank_with_history`
    // and call `rank` directly depend on this guarantee.
    suggestions.sort_by(|a, b| {
        let a_hist = a.source == SuggestionSource::History;
        let b_hist = b.source == SuggestionSource::History;
        a_hist
            .cmp(&b_hist)
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| priority::effective(b).cmp(&priority::effective(a)))
            .then_with(|| a.text.cmp(&b.text))
    });
    suggestions.truncate(max_results);
    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_history_ranks_below_equal_score_non_history() {
        use crate::types::{SuggestionKind, SuggestionSource};
        let items = vec![
            Suggestion {
                text: "checkout".to_string(),
                kind: SuggestionKind::History,
                source: SuggestionSource::History,
                ..Default::default()
            },
            Suggestion {
                text: "checkout".to_string(),
                kind: SuggestionKind::Subcommand,
                source: SuggestionSource::Spec,
                ..Default::default()
            },
        ];
        let result = rank("checkout", items, DEFAULT_MAX_RESULTS);
        // Same fuzzy score → priority breaks tie. Subcommand base 70 > History base 10.
        assert_eq!(result[0].source, SuggestionSource::Spec);
        assert_eq!(result[1].source, SuggestionSource::History);
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

    #[test]
    fn test_priority_overrides_kind_base() {
        use crate::priority::Priority;
        use crate::types::SuggestionKind;
        let items = vec![
            Suggestion {
                text: "A".to_string(),
                kind: SuggestionKind::Flag,
                priority: Some(Priority::new(95)),
                ..Default::default()
            },
            Suggestion {
                text: "B".to_string(),
                kind: SuggestionKind::GitBranch,
                priority: None,
                ..Default::default()
            },
        ];
        let result = rank("", items, DEFAULT_MAX_RESULTS);
        assert_eq!(
            result[0].text, "A",
            "spec priority 95 should beat branch base 80"
        );
    }

    #[test]
    fn test_history_base_keeps_history_last_on_empty_query() {
        // Texts chosen so alphabetical order CONTRADICTS priority order:
        // if the priority sort were ever bypassed, "a-history" would land
        // first via alphabetical fallback and this test would catch it.
        use crate::types::{SuggestionKind, SuggestionSource};
        let items = vec![
            Suggestion {
                text: "a-history".to_string(),
                kind: SuggestionKind::History,
                source: SuggestionSource::History,
                ..Default::default()
            },
            Suggestion {
                text: "z-flag".to_string(),
                kind: SuggestionKind::Flag,
                ..Default::default()
            },
        ];
        let result = rank("", items, DEFAULT_MAX_RESULTS);
        assert_eq!(
            result[0].text, "z-flag",
            "Flag base 30 should beat History base 10"
        );
        assert_eq!(result[1].text, "a-history");
    }
}
