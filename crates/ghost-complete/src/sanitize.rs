//! Terminal output sanitisation shared between CLI subcommands.
//!
//! File paths, config values, error strings, and detected-environment
//! identifiers all reach `println!` / `writeln!` unescaped. Any of them can
//! smuggle in CSI/OSC sequences — so strip control characters at the
//! print boundary, not per call-site. Mirrors `sanitize_text` in `gc-suggest`.

use std::path::Path;

/// Strip control characters (including ESC, BEL, NUL) from text headed for
/// the user's terminal. Preserves printable characters and valid UTF-8.
pub fn sanitize_for_terminal(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// Render a `Path` for terminal output, stripping control characters.
///
/// `Path::display()` returns a `Display` impl that prints the path verbatim
/// — including any embedded ESC/BEL/NUL bytes. A user whose `$HOME` or
/// config dir contains a crafted control sequence (unusual but possible;
/// macOS allows most bytes in filenames) would otherwise see that sequence
/// evaluated by the terminal. Route path renders through this helper so
/// the strip happens once at the print boundary.
pub fn sanitize_path(p: &Path) -> String {
    sanitize_for_terminal(&p.display().to_string())
}

/// Sanitise text while preserving ASCII whitespace (`\t`, `\n`, `\r`).
/// Used by the `config` dump, which prints a multi-line TOML document
/// verbatim — stripping every control char would collapse the whole
/// thing onto a single line. We still want ESC/BEL/NUL + every other
/// C0 + DEL + C1 gone, since `toml_edit` preserves comments and
/// string-literal trivia unchanged.
pub fn sanitize_preserving_whitespace(text: &str) -> String {
    text.chars()
        .filter(|&c| c == '\t' || c == '\n' || c == '\r' || !c.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_and_osc_sequences() {
        let hostile = "\x1b[31mred\x1b[0m\x1b]0;title\x07 ok";
        let cleaned = sanitize_for_terminal(hostile);
        assert_eq!(cleaned, "[31mred[0m]0;title ok");
    }

    #[test]
    fn preserves_plain_text() {
        let path = "/Users/alice/.config/ghost-complete/specs";
        assert_eq!(sanitize_for_terminal(path), path);
    }

    #[test]
    fn strips_newlines_and_nul() {
        assert_eq!(sanitize_for_terminal("a\nb\0c"), "abc");
    }

    #[test]
    fn preserving_whitespace_keeps_tabs_newlines_cr() {
        let text = "line1\nline2\tindented\r\nend";
        assert_eq!(sanitize_preserving_whitespace(text), text);
    }

    #[test]
    fn sanitize_path_strips_control_bytes() {
        use std::path::PathBuf;
        // Construct a path with an embedded ESC sequence — would normally
        // be evaluated by the terminal when we print it via display().
        let hostile = PathBuf::from("/Users/alice/\x1b[31mevil\x1b[0m/config");
        let cleaned = sanitize_path(&hostile);
        assert_eq!(cleaned, "/Users/alice/[31mevil[0m/config");
    }

    #[test]
    fn sanitize_path_preserves_plain_path() {
        use std::path::PathBuf;
        let path = PathBuf::from("/Users/alice/.config/ghost-complete/specs");
        assert_eq!(sanitize_path(&path), "/Users/alice/.config/ghost-complete/specs");
    }

    #[test]
    fn preserving_whitespace_strips_esc_bel_nul_and_c1() {
        let hostile = "safe\x1b[31m hostile\x07\x00 text \u{009b}x";
        // ESC, BEL, NUL, and the C1 CSI (U+009B) must all be stripped;
        // the non-control text (including CSI param bytes now standing
        // alone) remains.
        let cleaned = sanitize_preserving_whitespace(hostile);
        assert_eq!(cleaned, "safe[31m hostile text x");
    }
}
