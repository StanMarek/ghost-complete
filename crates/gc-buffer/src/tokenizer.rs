/// Shell-aware tokenizer that handles quoting, pipes, and redirects.
//
// Word boundaries use ASCII whitespace only (`ch.is_ascii_whitespace()`, i.e.
// space, tab, newline, carriage return, form feed, vertical tab). Unicode
// whitespace such as `\u{00A0}` (non-breaking space) or `\u{2003}` (em space)
// is intentionally treated as part of a word: real shells behave the same
// way because `IFS` defaults to `<space><tab><newline>`, and shell command
// lines in practice are ASCII-whitespace-separated. Do not "fix" this by
// swapping in `char::is_whitespace` — that would diverge from zsh/bash.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Word(String),
    Pipe,           // |
    And,            // &&
    Or,             // ||
    Semicolon,      // ;
    RedirectIn,     // <
    RedirectOut,    // >
    RedirectAppend, // >>
    Heredoc,        // <<
    HereString,     // <<<
    Background,     // &
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteState {
    None,
    SingleQuoted,
    DoubleQuoted,
}

pub struct TokenizeResult {
    pub tokens: Vec<Token>,
    pub quote_state: QuoteState,
    /// True when tokenization stopped at an unquoted `#` comment.
    pub in_comment: bool,
}

pub fn tokenize(input: &str) -> TokenizeResult {
    let mut tokens = Vec::new();
    let mut current_word = String::new();
    let mut quote_state = QuoteState::None;
    let mut in_comment = false;
    let mut chars = input.chars().peekable();

    while let Some(&ch) = chars.peek() {
        match quote_state {
            QuoteState::SingleQuoted => {
                chars.next();
                if ch == '\'' {
                    quote_state = QuoteState::None;
                } else {
                    current_word.push(ch);
                }
            }
            QuoteState::DoubleQuoted => {
                chars.next();
                if ch == '"' {
                    quote_state = QuoteState::None;
                } else if ch == '\\' {
                    if let Some(&next) = chars.peek() {
                        match next {
                            '"' | '\\' | '$' | '`' => {
                                current_word.push(next);
                                chars.next();
                            }
                            _ => {
                                current_word.push('\\');
                                current_word.push(next);
                                chars.next();
                            }
                        }
                    } else {
                        // Trailing backslash inside double quotes
                        current_word.push('\\');
                    }
                } else {
                    current_word.push(ch);
                }
            }
            QuoteState::None => {
                if ch == '\'' {
                    chars.next();
                    quote_state = QuoteState::SingleQuoted;
                } else if ch == '"' {
                    chars.next();
                    quote_state = QuoteState::DoubleQuoted;
                } else if ch == '\\' {
                    chars.next();
                    if let Some(&next) = chars.peek() {
                        current_word.push(next);
                        chars.next();
                    }
                } else if ch == '|' {
                    chars.next();
                    flush_word(&mut current_word, &mut tokens);
                    if chars.peek() == Some(&'|') {
                        chars.next();
                        tokens.push(Token::Or);
                    } else {
                        tokens.push(Token::Pipe);
                    }
                } else if ch == '&' {
                    chars.next();
                    if chars.peek() == Some(&'&') {
                        chars.next();
                        flush_word(&mut current_word, &mut tokens);
                        tokens.push(Token::And);
                    } else if chars.peek() == Some(&'>') {
                        // &> or &>> — redirect both stdout+stderr (bash/zsh shorthand)
                        chars.next();
                        flush_word(&mut current_word, &mut tokens);
                        if chars.peek() == Some(&'>') {
                            chars.next();
                            tokens.push(Token::RedirectAppend);
                        } else {
                            tokens.push(Token::RedirectOut);
                        }
                    } else if matches!(
                        tokens.last(),
                        Some(Token::RedirectOut | Token::RedirectAppend | Token::RedirectIn)
                    ) {
                        // & immediately after a redirect operator starts the target
                        // (e.g., 2>&1 — the &1 is the redirect target, not background)
                        current_word.push('&');
                    } else {
                        flush_word(&mut current_word, &mut tokens);
                        tokens.push(Token::Background);
                    }
                } else if ch == ';' {
                    chars.next();
                    flush_word(&mut current_word, &mut tokens);
                    tokens.push(Token::Semicolon);
                } else if ch == '#' && current_word.is_empty() {
                    // Unquoted # at a word boundary starts a comment — stop tokenizing
                    in_comment = true;
                    break;
                } else if ch == '>' {
                    chars.next();
                    // A word of all digits immediately before > is an FD number
                    // (e.g., 2>/dev/null, 10>/tmp/log), not an argument
                    if !current_word.is_empty() && current_word.bytes().all(|b| b.is_ascii_digit())
                    {
                        current_word.clear();
                    } else {
                        flush_word(&mut current_word, &mut tokens);
                    }
                    if chars.peek() == Some(&'>') {
                        chars.next();
                        tokens.push(Token::RedirectAppend);
                    } else {
                        tokens.push(Token::RedirectOut);
                    }
                } else if ch == '<' {
                    chars.next();
                    // Same FD number stripping for input redirects (e.g. 0<file, 3<file)
                    if !current_word.is_empty() && current_word.bytes().all(|b| b.is_ascii_digit())
                    {
                        current_word.clear();
                    } else {
                        flush_word(&mut current_word, &mut tokens);
                    }
                    if chars.peek() == Some(&'<') {
                        chars.next();
                        if chars.peek() == Some(&'<') {
                            chars.next();
                            tokens.push(Token::HereString);
                        } else {
                            // Consume optional '-' for <<- (tab-stripping heredoc)
                            if chars.peek() == Some(&'-') {
                                chars.next();
                            }
                            tokens.push(Token::Heredoc);
                        }
                    } else {
                        tokens.push(Token::RedirectIn);
                    }
                } else if ch.is_ascii_whitespace() {
                    chars.next();
                    flush_word(&mut current_word, &mut tokens);
                } else if ch == '$' {
                    chars.next();
                    current_word.push('$');
                    if chars.peek() == Some(&'(') {
                        chars.next();
                        current_word.push('(');
                        consume_subshell(&mut chars, &mut current_word);
                    }
                } else {
                    chars.next();
                    current_word.push(ch);
                }
            }
        }
    }

    // Flush any remaining word
    flush_word(&mut current_word, &mut tokens);

    TokenizeResult {
        tokens,
        quote_state,
        in_comment,
    }
}

fn flush_word(word: &mut String, tokens: &mut Vec<Token>) {
    if !word.is_empty() {
        tokens.push(Token::Word(std::mem::take(word)));
    }
}

/// Consume characters inside `$(...)`, tracking paren depth and respecting
/// quotes/escapes so that nested `)` inside strings don't close prematurely.
/// Called after `$(` has already been pushed into `word`.
fn consume_subshell(chars: &mut std::iter::Peekable<std::str::Chars>, word: &mut String) {
    let mut depth: u32 = 1;
    while let Some(&ch) = chars.peek() {
        chars.next();
        word.push(ch);
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return;
                }
            }
            '\'' => {
                // Consume until closing single quote
                for qch in chars.by_ref() {
                    word.push(qch);
                    if qch == '\'' {
                        break;
                    }
                }
            }
            '"' => {
                // Consume until closing double quote, handling backslash escapes
                while let Some(&qch) = chars.peek() {
                    chars.next();
                    word.push(qch);
                    if qch == '"' {
                        break;
                    } else if qch == '\\' {
                        if let Some(&ech) = chars.peek() {
                            chars.next();
                            word.push(ech);
                        }
                    }
                }
            }
            '\\' => {
                if let Some(&ech) = chars.peek() {
                    chars.next();
                    word.push(ech);
                }
            }
            _ => {}
        }
    }
    // Unmatched $( — input is incomplete, leave as-is
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(input: &str) -> Vec<Token> {
        tokenize(input).tokens
    }

    #[test]
    fn test_simple_command() {
        assert_eq!(
            words("ls -la"),
            vec![Token::Word("ls".into()), Token::Word("-la".into()),]
        );
    }

    #[test]
    fn test_pipe() {
        assert_eq!(
            words("cat f | grep x"),
            vec![
                Token::Word("cat".into()),
                Token::Word("f".into()),
                Token::Pipe,
                Token::Word("grep".into()),
                Token::Word("x".into()),
            ]
        );
    }

    #[test]
    fn test_redirect() {
        assert_eq!(
            words("echo hi > f.txt"),
            vec![
                Token::Word("echo".into()),
                Token::Word("hi".into()),
                Token::RedirectOut,
                Token::Word("f.txt".into()),
            ]
        );
    }

    #[test]
    fn test_append_redirect() {
        assert_eq!(
            words("echo hi >> f.txt"),
            vec![
                Token::Word("echo".into()),
                Token::Word("hi".into()),
                Token::RedirectAppend,
                Token::Word("f.txt".into()),
            ]
        );
    }

    #[test]
    fn test_single_quotes() {
        assert_eq!(
            words("echo 'hello world'"),
            vec![
                Token::Word("echo".into()),
                Token::Word("hello world".into()),
            ]
        );
    }

    #[test]
    fn test_double_quotes() {
        assert_eq!(
            words("echo \"hello world\""),
            vec![
                Token::Word("echo".into()),
                Token::Word("hello world".into()),
            ]
        );
    }

    #[test]
    fn test_escape_in_double_quotes() {
        assert_eq!(
            words(r#"echo "say \"hi\"""#),
            vec![Token::Word("echo".into()), Token::Word("say \"hi\"".into()),]
        );
    }

    #[test]
    fn test_backslash_escape() {
        assert_eq!(
            words(r"echo hello\ world"),
            vec![
                Token::Word("echo".into()),
                Token::Word("hello world".into()),
            ]
        );
    }

    #[test]
    fn test_and_operator() {
        assert_eq!(
            words("cmd1 && cmd2"),
            vec![
                Token::Word("cmd1".into()),
                Token::And,
                Token::Word("cmd2".into()),
            ]
        );
    }

    #[test]
    fn test_or_operator() {
        assert_eq!(
            words("cmd1 || cmd2"),
            vec![
                Token::Word("cmd1".into()),
                Token::Or,
                Token::Word("cmd2".into()),
            ]
        );
    }

    #[test]
    fn test_semicolon() {
        assert_eq!(
            words("cmd1; cmd2"),
            vec![
                Token::Word("cmd1".into()),
                Token::Semicolon,
                Token::Word("cmd2".into()),
            ]
        );
    }

    #[test]
    fn test_incomplete_double_quote() {
        let result = tokenize("echo \"hello");
        assert_eq!(
            result.tokens,
            vec![Token::Word("echo".into()), Token::Word("hello".into()),]
        );
        assert_eq!(result.quote_state, QuoteState::DoubleQuoted);
    }

    #[test]
    fn test_incomplete_single_quote() {
        let result = tokenize("echo 'hello");
        assert_eq!(
            result.tokens,
            vec![Token::Word("echo".into()), Token::Word("hello".into()),]
        );
        assert_eq!(result.quote_state, QuoteState::SingleQuoted);
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(words(""), Vec::<Token>::new());
    }

    #[test]
    fn test_only_spaces() {
        assert_eq!(words("   "), Vec::<Token>::new());
    }

    // --- comment handling ---

    #[test]
    fn test_comment_strips_trailing_words() {
        assert_eq!(
            words("ls -la # this is a comment"),
            vec![Token::Word("ls".into()), Token::Word("-la".into())]
        );
    }

    #[test]
    fn test_comment_at_start() {
        assert_eq!(words("# everything is a comment"), Vec::<Token>::new());
    }

    #[test]
    fn test_hash_inside_single_quotes_not_comment() {
        assert_eq!(
            words("echo '# not a comment'"),
            vec![
                Token::Word("echo".into()),
                Token::Word("# not a comment".into()),
            ]
        );
    }

    #[test]
    fn test_hash_inside_double_quotes_not_comment() {
        assert_eq!(
            words("echo \"# not a comment\""),
            vec![
                Token::Word("echo".into()),
                Token::Word("# not a comment".into()),
            ]
        );
    }

    #[test]
    fn test_hash_mid_word_not_comment() {
        // foo#bar is a single word in shell, not a comment
        assert_eq!(
            words("echo foo#bar"),
            vec![Token::Word("echo".into()), Token::Word("foo#bar".into()),]
        );
    }

    // --- FD redirect handling ---

    #[test]
    fn test_fd_redirect_stderr() {
        assert_eq!(
            words("cmd 2>/dev/null"),
            vec![
                Token::Word("cmd".into()),
                Token::RedirectOut,
                Token::Word("/dev/null".into()),
            ]
        );
    }

    #[test]
    fn test_fd_redirect_stderr_append() {
        assert_eq!(
            words("cmd 2>>log.txt"),
            vec![
                Token::Word("cmd".into()),
                Token::RedirectAppend,
                Token::Word("log.txt".into()),
            ]
        );
    }

    #[test]
    fn test_fd_redirect_stdin() {
        assert_eq!(
            words("cmd 0<input.txt"),
            vec![
                Token::Word("cmd".into()),
                Token::RedirectIn,
                Token::Word("input.txt".into()),
            ]
        );
    }

    #[test]
    fn test_fd_redirect_with_space_keeps_digit_as_arg() {
        // "cmd 2 >/dev/null" — the space means 2 is an argument, not FD
        assert_eq!(
            words("cmd 2 >/dev/null"),
            vec![
                Token::Word("cmd".into()),
                Token::Word("2".into()),
                Token::RedirectOut,
                Token::Word("/dev/null".into()),
            ]
        );
    }

    #[test]
    fn test_multi_digit_fd_stripped() {
        // "cmd 22>file" — 22 is an FD number, gets stripped like single-digit FDs
        assert_eq!(
            words("cmd 22>file"),
            vec![
                Token::Word("cmd".into()),
                Token::RedirectOut,
                Token::Word("file".into()),
            ]
        );
    }

    #[test]
    fn test_fd_redirect_multi_digit_stripped() {
        // 10>file — multi-digit FD should be stripped, not left as a word
        assert_eq!(
            words("cmd 10>file"),
            vec![
                Token::Word("cmd".into()),
                Token::RedirectOut,
                Token::Word("file".into()),
            ]
        );
    }

    #[test]
    fn test_fd_redirect_2_ampersand_1() {
        // 2>&1 — the & after > starts the redirect target, not background
        assert_eq!(
            words("cmd 2>&1"),
            vec![
                Token::Word("cmd".into()),
                Token::RedirectOut,
                Token::Word("&1".into()),
            ]
        );
    }

    #[test]
    fn test_ampersand_redirect_stdout_stderr() {
        // &>file — redirect both stdout and stderr
        assert_eq!(
            words("cmd &>file"),
            vec![
                Token::Word("cmd".into()),
                Token::RedirectOut,
                Token::Word("file".into()),
            ]
        );
    }

    #[test]
    fn test_ampersand_redirect_append() {
        // &>>file — append redirect both stdout and stderr
        assert_eq!(
            words("cmd &>>file"),
            vec![
                Token::Word("cmd".into()),
                Token::RedirectAppend,
                Token::Word("file".into()),
            ]
        );
    }

    #[test]
    fn test_background_still_works() {
        // Plain & should still be Background
        assert_eq!(
            words("cmd &"),
            vec![Token::Word("cmd".into()), Token::Background,]
        );
    }

    // --- LOW-1: heredoc / here-string ---

    #[test]
    fn test_heredoc() {
        assert_eq!(
            words("cat <<EOF"),
            vec![
                Token::Word("cat".into()),
                Token::Heredoc,
                Token::Word("EOF".into()),
            ]
        );
    }

    #[test]
    fn test_heredoc_tab_strip_variant() {
        // <<- is the tab-stripping heredoc; '-' is part of the operator, not the delimiter
        assert_eq!(
            words("cmd <<-EOF"),
            vec![
                Token::Word("cmd".into()),
                Token::Heredoc,
                Token::Word("EOF".into()),
            ]
        );
    }

    #[test]
    fn test_heredoc_not_two_redirect_in() {
        // Previously << was tokenized as RedirectIn, RedirectIn
        let tokens = words("cat <<EOF");
        assert!(!tokens.contains(&Token::RedirectIn));
    }

    #[test]
    fn test_here_string() {
        assert_eq!(
            words("cat <<<hello"),
            vec![
                Token::Word("cat".into()),
                Token::HereString,
                Token::Word("hello".into()),
            ]
        );
    }

    #[test]
    fn test_heredoc_with_redirect() {
        // Mixing heredoc with output redirect
        assert_eq!(
            words("cat <<EOF >out.txt"),
            vec![
                Token::Word("cat".into()),
                Token::Heredoc,
                Token::Word("EOF".into()),
                Token::RedirectOut,
                Token::Word("out.txt".into()),
            ]
        );
    }

    #[test]
    fn test_single_redirect_in_unchanged() {
        // Single < should still be RedirectIn
        assert_eq!(
            words("cmd <file"),
            vec![
                Token::Word("cmd".into()),
                Token::RedirectIn,
                Token::Word("file".into()),
            ]
        );
    }

    // --- LOW-2: $() command substitution ---

    #[test]
    fn test_command_substitution_single_word() {
        assert_eq!(
            words("echo $(whoami)"),
            vec![Token::Word("echo".into()), Token::Word("$(whoami)".into()),]
        );
    }

    #[test]
    fn test_command_substitution_with_spaces() {
        // Spaces inside $() should NOT split into separate tokens
        assert_eq!(
            words("echo $(git status)"),
            vec![
                Token::Word("echo".into()),
                Token::Word("$(git status)".into()),
            ]
        );
    }

    #[test]
    fn test_command_substitution_nested() {
        assert_eq!(
            words("echo $(cat $(find .))"),
            vec![
                Token::Word("echo".into()),
                Token::Word("$(cat $(find .))".into()),
            ]
        );
    }

    #[test]
    fn test_command_substitution_with_quotes_inside() {
        assert_eq!(
            words(r#"echo $(echo "hello world")"#),
            vec![
                Token::Word("echo".into()),
                Token::Word("$(echo \"hello world\")".into()),
            ]
        );
    }

    #[test]
    fn test_command_substitution_incomplete() {
        // Unclosed $( — should still be one token, not split on spaces
        assert_eq!(
            words("echo $(git status"),
            vec![
                Token::Word("echo".into()),
                Token::Word("$(git status".into()),
            ]
        );
    }

    #[test]
    fn test_dollar_without_paren_is_normal() {
        // Bare $ (like $HOME) should not trigger subshell consumption
        assert_eq!(
            words("echo $HOME"),
            vec![Token::Word("echo".into()), Token::Word("$HOME".into()),]
        );
    }

    #[test]
    fn test_command_substitution_mid_word() {
        // $() can appear mid-word
        assert_eq!(
            words("file-$(date +%s).txt"),
            vec![Token::Word("file-$(date +%s).txt".into())]
        );
    }

    #[test]
    fn test_command_substitution_with_pipe_inside() {
        // Pipe inside $() should not create a pipeline segment
        assert_eq!(
            words("echo $(ls | head)"),
            vec![
                Token::Word("echo".into()),
                Token::Word("$(ls | head)".into()),
            ]
        );
    }
}
