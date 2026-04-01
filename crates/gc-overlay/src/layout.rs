use gc_suggest::Suggestion;
use unicode_width::UnicodeWidthStr;

use crate::types::PopupLayout;
use crate::util::display_text;

#[allow(clippy::too_many_arguments)]
pub fn compute_layout(
    suggestions: &[Suggestion],
    scroll_offset: usize,
    cursor_row: u16,
    cursor_col: u16,
    _screen_rows: u16,
    screen_cols: u16,
    max_visible: usize,
    min_width: u16,
    max_width: u16,
) -> PopupLayout {
    let visible_count = suggestions.len().min(max_visible);

    // Compute width from visible suggestions (content only, no border)
    let content_width = suggestions
        .iter()
        .skip(scroll_offset)
        .take(max_visible)
        .map(item_display_width)
        .max()
        .unwrap_or((min_width - 2) as usize);
    // Add 2 for left/right border columns
    let width = (content_width as u16 + 2).clamp(min_width, max_width.min(screen_cols));

    // Height includes top + bottom border rows
    let height = (visible_count as u16) + 2;

    // Always render below cursor. Caller is responsible for adjusting
    // cursor_row via scroll deficit before calling this function.
    let start_row = cursor_row + 1;

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
        scroll_deficit: 0, // Caller sets this after computing scroll
    }
}

/// Calculate display width for a single suggestion line.
/// Format: " K text  description " where K is kind char.
fn item_display_width(suggestion: &Suggestion) -> usize {
    // gutter(" K ") = 3 chars, then text, then optional "  desc", then trailing space
    let (dt, _) = display_text(suggestion);
    let text_len = UnicodeWidthStr::width(dt);
    let desc_len = suggestion
        .description
        .as_ref()
        .map(|d| UnicodeWidthStr::width(d.as_str()) + 2) // 2 for gap before description
        .unwrap_or(0);
    3 + text_len + desc_len + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DEFAULT_MAX_POPUP_WIDTH, DEFAULT_MAX_VISIBLE, DEFAULT_MIN_POPUP_WIDTH};
    use gc_suggest::SuggestionKind;

    fn make(text: &str, desc: Option<&str>) -> Suggestion {
        Suggestion {
            text: text.to_string(),
            description: desc.map(String::from),
            ..Default::default()
        }
    }

    fn make_path(text: &str, kind: SuggestionKind, desc: Option<&str>) -> Suggestion {
        Suggestion {
            text: text.to_string(),
            kind,
            description: desc.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn test_popup_below_cursor() {
        let suggestions = vec![make("checkout", None), make("commit", None)];
        let layout = compute_layout(
            &suggestions,
            0,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
        );
        assert_eq!(layout.scroll_deficit, 0);
        assert_eq!(layout.start_row, 6);
    }

    #[test]
    fn test_popup_always_below_even_near_bottom() {
        let suggestions: Vec<Suggestion> =
            (0..5).map(|i| make(&format!("item{i}"), None)).collect();
        let layout = compute_layout(
            &suggestions,
            0,
            22,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
        );
        // Layout always places below — start_row = cursor_row + 1
        assert_eq!(layout.start_row, 23);
        assert_eq!(layout.scroll_deficit, 0);
    }

    #[test]
    fn test_popup_shifts_left() {
        let suggestions = vec![make("a-long-suggestion-name", None)];
        let layout = compute_layout(
            &suggestions,
            0,
            5,
            70,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
        );
        assert!(layout.start_col + layout.width <= 80);
    }

    #[test]
    fn test_popup_at_top_left() {
        let suggestions = vec![make("ls", None)];
        let layout = compute_layout(
            &suggestions,
            0,
            0,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
        );
        assert_eq!(layout.start_row, 1);
        assert_eq!(layout.start_col, 0);
    }

    #[test]
    fn test_width_clamped_min() {
        let suggestions = vec![make("x", None)];
        let layout = compute_layout(
            &suggestions,
            0,
            0,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
        );
        assert!(layout.width >= DEFAULT_MIN_POPUP_WIDTH);
    }

    #[test]
    fn test_width_clamped_max() {
        let long_desc = "a".repeat(200);
        let suggestions = vec![make("cmd", Some(&long_desc))];
        let layout = compute_layout(
            &suggestions,
            0,
            0,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
        );
        assert!(layout.width <= DEFAULT_MAX_POPUP_WIDTH);
    }

    #[test]
    fn test_height_capped_at_max_visible() {
        let suggestions: Vec<Suggestion> =
            (0..50).map(|i| make(&format!("item{i}"), None)).collect();
        let layout = compute_layout(
            &suggestions,
            0,
            0,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
        );
        assert_eq!(layout.height, DEFAULT_MAX_VISIBLE as u16 + 2); // +2 for borders
    }

    #[test]
    fn test_custom_max_visible() {
        let suggestions: Vec<Suggestion> =
            (0..50).map(|i| make(&format!("item{i}"), None)).collect();
        let layout = compute_layout(
            &suggestions,
            0,
            0,
            0,
            24,
            80,
            5,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
        );
        assert_eq!(layout.height, 7); // 5 content + 2 borders
    }

    // --- Bug B4: filepath width uses basename only ---

    #[test]
    fn test_filepath_width_uses_basename() {
        // "src/deeply/nested/file.txt" as FilePath — display width should be
        // based on "file.txt" (8 chars), not the full 25-byte path.
        let deep = make_path("src/deeply/nested/file.txt", SuggestionKind::FilePath, None);
        let shallow = make_path("file.txt", SuggestionKind::FilePath, None);
        assert_eq!(item_display_width(&deep), item_display_width(&shallow));
    }

    #[test]
    fn test_directory_width_uses_basename() {
        // "path/to/mydir/" as Directory — display should be "mydir/" (6 chars).
        let deep = make_path("path/to/mydir/", SuggestionKind::Directory, None);
        // gutter(3) + "mydir/"(6) + trailing(1) = 10
        assert_eq!(item_display_width(&deep), 3 + 6 + 1);
    }

    #[test]
    fn test_filepath_no_slash_unchanged() {
        let s = make_path("Cargo.toml", SuggestionKind::FilePath, None);
        // gutter(3) + "Cargo.toml"(10) + trailing(1) = 14
        assert_eq!(item_display_width(&s), 3 + 10 + 1);
    }

    // --- Bug B5: non-ASCII char counting ---

    #[test]
    fn test_non_ascii_text_width() {
        // 3 CJK characters = 6 terminal columns (2 each via unicode-width)
        let s = make("\u{65E5}\u{672C}\u{8A9E}", None);
        // gutter(3) + text(6 cols) + trailing(1) = 10
        assert_eq!(item_display_width(&s), 3 + 6 + 1);
    }

    #[test]
    fn test_non_ascii_description_width() {
        // 3 accented chars = 3 terminal columns (1 each, not fullwidth)
        let s = make("cmd", Some("\u{00E9}\u{00E8}\u{00EA}"));
        // gutter(3) + "cmd"(3) + gap(2) + desc(3 cols) + trailing(1) = 12
        assert_eq!(item_display_width(&s), 3 + 3 + 2 + 3 + 1);
    }

    #[test]
    fn test_non_ascii_filepath_width() {
        // basename "ファイル.txt": 4 katakana (2 cols each = 8) + ".txt" (4) = 12 cols
        let s = make_path(
            "docs/\u{65E5}\u{672C}\u{8A9E}/\u{30D5}\u{30A1}\u{30A4}\u{30EB}.txt",
            SuggestionKind::FilePath,
            None,
        );
        // gutter(3) + basename(12 cols) + trailing(1) = 16
        assert_eq!(item_display_width(&s), 3 + 12 + 1);
    }
}
