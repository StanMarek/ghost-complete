use gc_suggest::Suggestion;

use crate::types::PopupLayout;

pub fn compute_layout(
    suggestions: &[Suggestion],
    scroll_offset: usize,
    cursor_row: u16,
    cursor_col: u16,
    screen_rows: u16,
    screen_cols: u16,
    max_visible: usize,
    min_width: u16,
    max_width: u16,
) -> PopupLayout {
    let visible_count = suggestions.len().min(max_visible);
    let height = visible_count as u16;

    // Compute width from visible suggestions
    let content_width = suggestions
        .iter()
        .skip(scroll_offset)
        .take(max_visible)
        .map(item_display_width)
        .max()
        .unwrap_or(min_width as usize);
    let width = (content_width as u16).clamp(min_width, max_width.min(screen_cols));

    // Vertical: prefer below cursor, above if not enough space
    let space_below = screen_rows.saturating_sub(cursor_row + 1);
    let renders_above = space_below < height;
    let start_row = if renders_above {
        cursor_row.saturating_sub(height)
    } else {
        cursor_row + 1
    };

    // Horizontal: start at cursor col, shift left if overflows
    let start_col = if cursor_col + width > screen_cols {
        screen_cols.saturating_sub(width)
    } else {
        cursor_col
    };

    PopupLayout {
        start_row,
        start_col,
        width,
        height,
        renders_above,
    }
}

/// Calculate display width for a single suggestion line.
/// Format: " K text  description " where K is kind char.
fn item_display_width(suggestion: &Suggestion) -> usize {
    // gutter(" K ") = 3 chars, then text, then optional "  desc", then trailing space
    let text_len = suggestion.text.len();
    let desc_len = suggestion
        .description
        .as_ref()
        .map(|d| d.len() + 2) // 2 for gap before description
        .unwrap_or(0);
    3 + text_len + desc_len + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DEFAULT_MAX_POPUP_WIDTH, DEFAULT_MAX_VISIBLE, DEFAULT_MIN_POPUP_WIDTH};
    use gc_suggest::{SuggestionKind, SuggestionSource};

    fn make(text: &str, desc: Option<&str>) -> Suggestion {
        Suggestion {
            text: text.to_string(),
            description: desc.map(String::from),
            kind: SuggestionKind::Command,
            source: SuggestionSource::Commands,
            score: 0,
        }
    }

    #[test]
    fn test_popup_below_cursor() {
        let suggestions = vec![make("checkout", None), make("commit", None)];
        let layout = compute_layout(
            &suggestions, 0, 5, 0, 24, 80,
            DEFAULT_MAX_VISIBLE, DEFAULT_MIN_POPUP_WIDTH, DEFAULT_MAX_POPUP_WIDTH,
        );
        assert!(!layout.renders_above);
        assert_eq!(layout.start_row, 6);
    }

    #[test]
    fn test_popup_above_cursor() {
        let suggestions: Vec<Suggestion> =
            (0..5).map(|i| make(&format!("item{i}"), None)).collect();
        let layout = compute_layout(
            &suggestions, 0, 22, 0, 24, 80,
            DEFAULT_MAX_VISIBLE, DEFAULT_MIN_POPUP_WIDTH, DEFAULT_MAX_POPUP_WIDTH,
        );
        assert!(layout.renders_above);
        assert!(layout.start_row < 22);
    }

    #[test]
    fn test_popup_shifts_left() {
        let suggestions = vec![make("a-long-suggestion-name", None)];
        let layout = compute_layout(
            &suggestions, 0, 5, 70, 24, 80,
            DEFAULT_MAX_VISIBLE, DEFAULT_MIN_POPUP_WIDTH, DEFAULT_MAX_POPUP_WIDTH,
        );
        assert!(layout.start_col + layout.width <= 80);
    }

    #[test]
    fn test_popup_at_top_left() {
        let suggestions = vec![make("ls", None)];
        let layout = compute_layout(
            &suggestions, 0, 0, 0, 24, 80,
            DEFAULT_MAX_VISIBLE, DEFAULT_MIN_POPUP_WIDTH, DEFAULT_MAX_POPUP_WIDTH,
        );
        assert_eq!(layout.start_row, 1);
        assert_eq!(layout.start_col, 0);
    }

    #[test]
    fn test_width_clamped_min() {
        let suggestions = vec![make("x", None)];
        let layout = compute_layout(
            &suggestions, 0, 0, 0, 24, 80,
            DEFAULT_MAX_VISIBLE, DEFAULT_MIN_POPUP_WIDTH, DEFAULT_MAX_POPUP_WIDTH,
        );
        assert!(layout.width >= DEFAULT_MIN_POPUP_WIDTH);
    }

    #[test]
    fn test_width_clamped_max() {
        let long_desc = "a".repeat(200);
        let suggestions = vec![make("cmd", Some(&long_desc))];
        let layout = compute_layout(
            &suggestions, 0, 0, 0, 24, 80,
            DEFAULT_MAX_VISIBLE, DEFAULT_MIN_POPUP_WIDTH, DEFAULT_MAX_POPUP_WIDTH,
        );
        assert!(layout.width <= DEFAULT_MAX_POPUP_WIDTH);
    }

    #[test]
    fn test_height_capped_at_max_visible() {
        let suggestions: Vec<Suggestion> =
            (0..50).map(|i| make(&format!("item{i}"), None)).collect();
        let layout = compute_layout(
            &suggestions, 0, 0, 0, 24, 80,
            DEFAULT_MAX_VISIBLE, DEFAULT_MIN_POPUP_WIDTH, DEFAULT_MAX_POPUP_WIDTH,
        );
        assert_eq!(layout.height, DEFAULT_MAX_VISIBLE as u16);
    }

    #[test]
    fn test_custom_max_visible() {
        let suggestions: Vec<Suggestion> =
            (0..50).map(|i| make(&format!("item{i}"), None)).collect();
        let layout = compute_layout(
            &suggestions, 0, 0, 0, 24, 80,
            5, DEFAULT_MIN_POPUP_WIDTH, DEFAULT_MAX_POPUP_WIDTH,
        );
        assert_eq!(layout.height, 5);
    }
}
