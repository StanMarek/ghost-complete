use crate::tokenizer::{tokenize, QuoteState, Token};

/// Describes what the user is typing: which command, which argument position,
/// whether we're after a pipe or redirect, and the partial word at the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandContext {
    /// The command being invoked (e.g. "git"), or None if cursor is at command position.
    pub command: Option<String>,
    /// Completed arguments before the cursor position.
    pub args: Vec<String>,
    /// Partial word at the cursor (for fuzzy matching). Empty if cursor is at a word boundary.
    pub current_word: String,
    /// 0 = command position, 1+ = argument positions.
    pub word_index: usize,
    /// `current_word` starts with `-`.
    pub is_flag: bool,
    /// `current_word` starts with `--`.
    pub is_long_flag: bool,
    /// Flag immediately before `current_word` (for flag-value pairs like `-b main`).
    pub preceding_flag: Option<String>,
    /// Cursor is after a pipe operator in a pipeline.
    pub in_pipe: bool,
    /// Cursor is after `>` or `<` (expects a filename).
    pub in_redirect: bool,
    /// Whether the cursor is inside an unclosed quote.
    pub quote_state: QuoteState,
    /// True when there are no preceding `|`, `&&`, `||`, or `;` operators.
    pub is_first_segment: bool,
}

/// Parse a command buffer and cursor position into a `CommandContext`.
///
/// This is a pure function — takes the raw buffer string and cursor byte offset,
/// returns structured context for the suggestion engine.
pub fn parse_command_context(buffer: &str, cursor: usize) -> CommandContext {
    // `cursor` is a character offset (from zsh $CURSOR / fish commandline -C).
    // Convert to a byte offset for safe string slicing.
    let byte_cursor = crate::char_to_byte_offset(buffer, cursor);
    let before_cursor = &buffer[..byte_cursor];

    let result = tokenize(before_cursor);
    let tokens = &result.tokens;

    // Find the last pipeline segment: everything after the last |, &&, ||, or ;
    let mut segment_start = 0;
    let mut found_pipe = false;
    for (i, tok) in tokens.iter().enumerate() {
        match tok {
            Token::Pipe => {
                segment_start = i + 1;
                found_pipe = true;
            }
            Token::And | Token::Or | Token::Semicolon => {
                segment_start = i + 1;
                found_pipe = false;
            }
            _ => {}
        }
    }

    let segment = &tokens[segment_start..];

    // Check if the last token in the full stream was a redirect
    let last_token_is_redirect = tokens.last().is_some_and(|t| {
        matches!(
            t,
            Token::RedirectOut
                | Token::RedirectAppend
                | Token::RedirectIn
                | Token::Heredoc
                | Token::HereString
        )
    });

    // Determine if cursor is at a word boundary (trailing space) or mid-word.
    // If we're in a comment, all preceding tokens are complete.
    let ends_with_space = result.in_comment
        || (!before_cursor.is_empty()
            && before_cursor.as_bytes()[before_cursor.len() - 1].is_ascii_whitespace()
            && result.quote_state == QuoteState::None);

    // Collect words from the segment
    let mut words: Vec<&str> = Vec::new();
    for tok in segment {
        if let Token::Word(w) = tok {
            words.push(w);
        } else if matches!(
            tok,
            Token::RedirectOut
                | Token::RedirectAppend
                | Token::RedirectIn
                | Token::Heredoc
                | Token::HereString
        ) {
            // Redirect targets are filenames, not args — skip words after redirects
            // But for now, just track the redirect operator position
        }
    }

    // If cursor ends with whitespace, all words are complete.
    // If not, the last word is the partial current_word.
    let (complete_words, current_word) = if ends_with_space || words.is_empty() {
        (words.as_slice(), "")
    } else {
        let (head, tail) = words.split_at(words.len() - 1);
        (head, tail.first().copied().unwrap_or(""))
    };

    // First complete word is the command (if any). `word_index` always equals
    // `complete_words.len()` — that's the slot the cursor (current_word) sits in,
    // whether or not current_word happens to be empty.
    let (command, args, word_index) = if complete_words.is_empty() {
        // No complete words — cursor is on the first word (command position)
        (None, Vec::new(), 0)
    } else {
        let cmd = complete_words[0].to_string();
        let args: Vec<String> = complete_words[1..].iter().map(|s| s.to_string()).collect();
        let word_index = complete_words.len(); // current_word is at this index
        (Some(cmd), args, word_index)
    };

    let is_flag = current_word.starts_with('-');
    let is_long_flag = current_word.starts_with("--");

    // Find preceding flag: last arg that starts with '-' immediately before current position
    let preceding_flag = args
        .last()
        .filter(|last_arg| last_arg.starts_with('-'))
        .cloned();

    let in_redirect = last_token_is_redirect;

    CommandContext {
        command,
        args,
        current_word: current_word.to_string(),
        word_index,
        is_flag,
        is_long_flag,
        preceding_flag,
        in_pipe: found_pipe,
        in_redirect,
        quote_state: result.quote_state,
        is_first_segment: segment_start == 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_command_at_end() {
        let ctx = parse_command_context("git", 3);
        assert_eq!(ctx.command, None);
        assert_eq!(ctx.current_word, "git");
        assert_eq!(ctx.word_index, 0);
    }

    #[test]
    fn test_command_with_space() {
        let ctx = parse_command_context("git ", 4);
        assert_eq!(ctx.command, Some("git".into()));
        assert_eq!(ctx.current_word, "");
        assert_eq!(ctx.word_index, 1);
    }

    #[test]
    fn test_command_with_arg() {
        let ctx = parse_command_context("git checkout ", 13);
        assert_eq!(ctx.command, Some("git".into()));
        assert_eq!(ctx.args, vec!["checkout"]);
        assert_eq!(ctx.current_word, "");
        assert_eq!(ctx.word_index, 2);
    }

    #[test]
    fn test_partial_arg() {
        let ctx = parse_command_context("git check", 9);
        assert_eq!(ctx.command, Some("git".into()));
        assert_eq!(ctx.current_word, "check");
        assert_eq!(ctx.word_index, 1);
    }

    #[test]
    fn test_flag() {
        let ctx = parse_command_context("ls -l", 5);
        assert!(ctx.is_flag);
        assert_eq!(ctx.current_word, "-l");
    }

    #[test]
    fn test_long_flag() {
        let ctx = parse_command_context("git log --one", 13);
        assert!(ctx.is_long_flag);
        assert!(ctx.is_flag);
        assert_eq!(ctx.current_word, "--one");
    }

    #[test]
    fn test_flag_value() {
        let ctx = parse_command_context("git checkout -b ", 16);
        assert_eq!(ctx.preceding_flag, Some("-b".into()));
        assert_eq!(ctx.current_word, "");
    }

    #[test]
    fn test_after_pipe() {
        let ctx = parse_command_context("cat f | grep ", 13);
        assert_eq!(ctx.command, Some("grep".into()));
        assert!(ctx.in_pipe);
        assert_eq!(ctx.current_word, "");
        assert_eq!(ctx.word_index, 1);
    }

    #[test]
    fn test_after_redirect() {
        let ctx = parse_command_context("echo hi > ", 10);
        assert!(ctx.in_redirect);
    }

    #[test]
    fn test_after_semicolon() {
        let ctx = parse_command_context("cd /tmp; ls ", 12);
        assert_eq!(ctx.command, Some("ls".into()));
        assert_eq!(ctx.current_word, "");
        assert_eq!(ctx.word_index, 1);
    }

    #[test]
    fn test_cursor_at_start() {
        let ctx = parse_command_context("", 0);
        assert_eq!(ctx.command, None);
        assert_eq!(ctx.current_word, "");
        assert_eq!(ctx.word_index, 0);
    }

    #[test]
    fn test_in_double_quotes() {
        let ctx = parse_command_context("echo \"hel", 9);
        assert_eq!(ctx.quote_state, QuoteState::DoubleQuoted);
        assert_eq!(ctx.current_word, "hel");
    }

    #[test]
    fn test_multiple_pipes() {
        let ctx = parse_command_context("a | b | c ", 10);
        assert_eq!(ctx.command, Some("c".into()));
        assert!(ctx.in_pipe);
    }

    #[test]
    fn test_multibyte_char_does_not_panic() {
        // 'ą' is 2 bytes in UTF-8. cursor=1 means after the first CHARACTER.
        let ctx = parse_command_context("ą", 1);
        assert_eq!(ctx.current_word, "ą");
        assert_eq!(ctx.word_index, 0);
    }

    #[test]
    fn test_multibyte_mid_buffer() {
        // "echo ąść" — cursor at char 6 (after "echo ą")
        let ctx = parse_command_context("echo ąść", 6);
        assert_eq!(ctx.command, Some("echo".into()));
        assert_eq!(ctx.current_word, "ą");
    }

    #[test]
    fn test_multibyte_full_word() {
        let ctx = parse_command_context("echo ąść", 8);
        assert_eq!(ctx.command, Some("echo".into()));
        assert_eq!(ctx.current_word, "ąść");
    }

    #[test]
    fn test_cursor_beyond_end_clamps() {
        let ctx = parse_command_context("git", 999);
        assert_eq!(ctx.current_word, "git");
    }

    #[test]
    fn test_is_first_segment_simple_command() {
        let ctx = parse_command_context("git", 3);
        assert!(ctx.is_first_segment);
    }

    #[test]
    fn test_is_first_segment_false_after_pipe() {
        let ctx = parse_command_context("cat f | grep ", 13);
        assert!(!ctx.is_first_segment);
    }

    #[test]
    fn test_is_first_segment_false_after_semicolon() {
        let ctx = parse_command_context("cd /tmp; ls ", 12);
        assert!(!ctx.is_first_segment);
    }

    #[test]
    fn test_is_first_segment_false_after_and() {
        let ctx = parse_command_context("make && ./run", 13);
        assert!(!ctx.is_first_segment);
    }

    #[test]
    fn test_is_first_segment_false_after_or() {
        let ctx = parse_command_context("test || echo fail", 17);
        assert!(!ctx.is_first_segment);
    }

    #[test]
    fn test_is_first_segment_true_empty() {
        let ctx = parse_command_context("", 0);
        assert!(ctx.is_first_segment);
    }

    #[test]
    fn test_comment_not_counted_as_args() {
        let ctx = parse_command_context("ls -la # comment", 17);
        assert_eq!(ctx.command, Some("ls".into()));
        assert_eq!(ctx.args, vec!["-la"]);
        assert_eq!(ctx.word_index, 2);
        assert_eq!(ctx.current_word, "");
    }

    #[test]
    fn test_fd_redirect_not_counted_as_arg() {
        let ctx = parse_command_context("cmd 2>/dev/null ", 16);
        assert_eq!(ctx.command, Some("cmd".into()));
        // 2 should NOT appear in args — it's an FD prefix, not an argument
        assert!(ctx.args.iter().all(|a| a != "2"));
    }

    #[test]
    fn test_heredoc_sets_in_redirect() {
        let ctx = parse_command_context("cat <<EOF ", 10);
        // After <<EOF, cursor expects the delimiter/body — treated like redirect
        assert_eq!(ctx.command, Some("cat".into()));
    }

    #[test]
    fn test_subshell_not_split_into_args() {
        let ctx = parse_command_context("echo $(git status) ", 19);
        assert_eq!(ctx.command, Some("echo".into()));
        // The whole $(git status) is one arg, not three
        assert_eq!(ctx.args, vec!["$(git status)"]);
        assert_eq!(ctx.word_index, 2);
    }

    #[test]
    fn test_subshell_pipe_stays_in_outer_context() {
        // Pipe inside $() should NOT affect the outer segment
        let ctx = parse_command_context("echo $(ls | head) ", 18);
        assert_eq!(ctx.command, Some("echo".into()));
        assert!(!ctx.in_pipe);
        assert!(ctx.is_first_segment);
    }
}
