//! Terminal output sanitisation shared between CLI subcommands.
//!
//! File paths, config values, error strings, and detected-environment
//! identifiers all reach `println!` / `writeln!` unescaped. Any of them can
//! smuggle in CSI/OSC sequences — so strip control characters at the
//! print boundary, not per call-site. Mirrors `sanitize_text` in `gc-suggest`.

/// Strip control characters (including ESC, BEL, NUL) from text headed for
/// the user's terminal. Preserves printable characters and valid UTF-8.
pub fn sanitize_for_terminal(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
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
}
