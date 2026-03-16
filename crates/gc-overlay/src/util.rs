use gc_suggest::{Suggestion, SuggestionKind};

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
}
