use gc_suggest::{Suggestion, SuggestionKind};
use unicode_width::UnicodeWidthChar;

/// Single-column ellipsis glyph appended when a description is truncated.
pub(crate) const TRUNCATION_ELLIPSIS: char = '\u{2026}';

/// Truncate `s` to `max_cols` display columns, appending an ellipsis (`…`)
/// when truncation actually happened. Returns the produced string and the
/// number of display columns it occupies.
///
/// Width is measured in terminal columns via `unicode_width`, not bytes or
/// chars. Zero-width chars (combining marks) are preserved without extending
/// the column count. Wide chars (CJK) consume 2 columns each.
///
/// When `max_cols == 0`, returns an empty string. When the input fits within
/// `max_cols`, no ellipsis is appended and the string is returned verbatim.
pub(crate) fn truncate_with_ellipsis(s: &str, max_cols: usize) -> (String, usize) {
    if max_cols == 0 {
        return (String::new(), 0);
    }
    let mut out = String::new();
    let mut cols: usize = 0;
    for ch in s.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if cols + w > max_cols {
            // Need to truncate. Strip back to leave room for the 1-col
            // ellipsis, then append it.
            let budget = max_cols.saturating_sub(1);
            while cols > budget {
                let Some(last) = out.pop() else {
                    break;
                };
                cols = cols.saturating_sub(UnicodeWidthChar::width(last).unwrap_or(0));
            }
            out.push(TRUNCATION_ELLIPSIS);
            cols += 1;
            return (out, cols);
        }
        out.push(ch);
        cols += w;
    }
    (out, cols)
}

/// Return the display text for a suggestion (basename for paths, full text otherwise)
/// and the number of *characters* in the stripped prefix (used to offset match indices).
///
/// For `FilePath` and `Directory` suggestions the popup only shows the last
/// path component (basename) because the user already typed the directory
/// prefix. This function centralises that logic so both `layout.rs` (width
/// calculation) and `render.rs` (rendering) stay in sync.
pub(crate) fn display_text(s: &Suggestion) -> (&str, usize) {
    match s.kind {
        SuggestionKind::FilePath | SuggestionKind::Directory => {
            let trimmed = s.text.trim_end_matches('/');
            match trimmed.rfind('/') {
                Some(byte_idx) => (
                    &s.text[byte_idx + 1..],
                    s.text[..byte_idx + 1].chars().count(),
                ),
                None => (&s.text[..], 0),
            }
        }
        _ => (&s.text[..], 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggestion(text: &str, kind: SuggestionKind) -> Suggestion {
        Suggestion {
            text: text.to_string(),
            kind,
            ..Default::default()
        }
    }

    #[test]
    fn plain_command_returns_full_text() {
        let s = suggestion("checkout", SuggestionKind::Command);
        let (dt, prefix) = display_text(&s);
        assert_eq!(dt, "checkout");
        assert_eq!(prefix, 0);
    }

    #[test]
    fn filepath_returns_basename() {
        let s = suggestion("src/main.rs", SuggestionKind::FilePath);
        let (dt, prefix) = display_text(&s);
        assert_eq!(dt, "main.rs");
        assert_eq!(prefix, 4); // "src/" is 4 chars
    }

    #[test]
    fn directory_with_trailing_slash() {
        let s = suggestion("path/to/dir/", SuggestionKind::Directory);
        let (dt, prefix) = display_text(&s);
        assert_eq!(dt, "dir/");
        assert_eq!(prefix, 8); // "path/to/" is 8 chars
    }

    #[test]
    fn filepath_no_slash() {
        let s = suggestion("Cargo.toml", SuggestionKind::FilePath);
        let (dt, prefix) = display_text(&s);
        assert_eq!(dt, "Cargo.toml");
        assert_eq!(prefix, 0);
    }

    #[test]
    fn deep_path() {
        let s = suggestion("a/b/c/d/e/file.txt", SuggestionKind::FilePath);
        let (dt, prefix) = display_text(&s);
        assert_eq!(dt, "file.txt");
        assert_eq!(prefix, 10); // "a/b/c/d/e/" is 10 chars
    }

    #[test]
    fn non_ascii_filepath() {
        // Japanese characters in path
        let s = suggestion(
            "docs/\u{65E5}\u{672C}\u{8A9E}/\u{30D5}\u{30A1}\u{30A4}\u{30EB}.txt",
            SuggestionKind::FilePath,
        );
        let (dt, prefix) = display_text(&s);
        assert_eq!(dt, "\u{30D5}\u{30A1}\u{30A4}\u{30EB}.txt");
        // "docs/\u{65E5}\u{672C}\u{8A9E}/" = 9 chars
        assert_eq!(prefix, 9);
    }

    #[test]
    fn truncate_ellipsis_short_input_unchanged() {
        let (out, cols) = truncate_with_ellipsis("hello", 10);
        assert_eq!(out, "hello");
        assert_eq!(cols, 5);
    }

    #[test]
    fn truncate_ellipsis_exact_fit_unchanged() {
        let (out, cols) = truncate_with_ellipsis("hello", 5);
        assert_eq!(out, "hello");
        assert_eq!(cols, 5);
    }

    #[test]
    fn truncate_ellipsis_truncates_and_appends() {
        let (out, cols) = truncate_with_ellipsis("hello world", 8);
        assert_eq!(out, "hello w\u{2026}");
        assert_eq!(cols, 8);
    }

    #[test]
    fn truncate_ellipsis_zero_max_cols_empty() {
        let (out, cols) = truncate_with_ellipsis("anything", 0);
        assert!(out.is_empty());
        assert_eq!(cols, 0);
    }

    #[test]
    fn truncate_ellipsis_one_max_col_yields_just_ellipsis_when_truncating() {
        // "ab" doesn't fit in 1 col; we strip to budget 0 then push the ellipsis.
        let (out, cols) = truncate_with_ellipsis("ab", 1);
        assert_eq!(out, "\u{2026}");
        assert_eq!(cols, 1);
    }

    #[test]
    fn truncate_ellipsis_handles_cjk_width() {
        // 4 CJK chars × 2 cols = 8 cols. Fits in 8.
        let (out, cols) = truncate_with_ellipsis("\u{65E5}\u{672C}\u{8A9E}\u{6F22}", 8);
        assert_eq!(cols, 8);
        assert_eq!(out, "\u{65E5}\u{672C}\u{8A9E}\u{6F22}");
    }

    #[test]
    fn truncate_ellipsis_strips_wide_char_for_ellipsis_room() {
        // 4 CJK chars in 5 cols: a CJK is 2 cols and the ellipsis is 1, so we
        // can fit at most 2 CJK chars (4 cols) + ellipsis (1 col) = 5 cols.
        let (out, cols) = truncate_with_ellipsis("\u{65E5}\u{672C}\u{8A9E}\u{6F22}", 5);
        assert_eq!(out, "\u{65E5}\u{672C}\u{2026}");
        assert_eq!(cols, 5);
    }

    #[test]
    fn truncate_ellipsis_zero_width_char_doesnt_extend() {
        // U+0301 (combining acute) is zero-width; it rides along with the
        // preceding char without consuming a column.
        let (out, cols) = truncate_with_ellipsis("a\u{0301}bc", 5);
        assert_eq!(out, "a\u{0301}bc");
        assert_eq!(cols, 3);
    }
}
