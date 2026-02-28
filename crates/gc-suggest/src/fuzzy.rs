use nucleo::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32String};

use crate::types::Suggestion;

const MAX_RESULTS: usize = 50;

pub fn rank(query: &str, mut suggestions: Vec<Suggestion>) -> Vec<Suggestion> {
    if query.is_empty() {
        suggestions.truncate(MAX_RESULTS);
        return suggestions;
    }

    let pattern = Pattern::new(
        query,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut matcher = Matcher::new(Config::DEFAULT);

    suggestions.retain_mut(|s| {
        let haystack = Utf32String::from(s.text.as_str());
        match pattern.score(haystack.slice(..), &mut matcher) {
            Some(score) => {
                s.score = score;
                true
            }
            None => false,
        }
    });

    suggestions.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.text.cmp(&b.text)));
    suggestions.truncate(MAX_RESULTS);
    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{SuggestionKind, SuggestionSource};

    fn make(text: &str) -> Suggestion {
        Suggestion {
            text: text.to_string(),
            description: None,
            kind: SuggestionKind::Command,
            source: SuggestionSource::Commands,
            score: 0,
        }
    }

    #[test]
    fn test_empty_query_returns_all() {
        let items: Vec<Suggestion> = (0..10).map(|i| make(&format!("item{i}"))).collect();
        let result = rank("", items);
        assert_eq!(result.len(), 10);
    }

    #[test]
    fn test_fuzzy_match_filters() {
        let items = vec![make("checkout"), make("cherry-pick"), make("zzzzz")];
        let result = rank("che", items);
        assert!(result.iter().any(|s| s.text == "checkout"));
        assert!(result.iter().any(|s| s.text == "cherry-pick"));
        assert!(!result.iter().any(|s| s.text == "zzzzz"));
    }

    #[test]
    fn test_exact_prefix_scores_higher() {
        let items = vec![make("achievement"), make("checkout")];
        let result = rank("check", items);
        assert!(!result.is_empty());
        assert_eq!(result[0].text, "checkout");
    }

    #[test]
    fn test_no_matches_returns_empty() {
        let items = vec![make("alpha"), make("beta"), make("gamma")];
        let result = rank("zzzzxxx", items);
        assert!(result.is_empty());
    }

    #[test]
    fn test_max_results_cap() {
        let items: Vec<Suggestion> = (0..100).map(|i| make(&format!("item{i}"))).collect();
        let result = rank("item", items);
        assert!(result.len() <= MAX_RESULTS);
    }

    #[test]
    fn test_scores_are_set() {
        let items = vec![make("checkout"), make("cherry-pick")];
        let result = rank("ch", items);
        for s in &result {
            assert!(s.score > 0, "score should be > 0 after ranking");
        }
    }
}
