/// Shell-aware tokenizer that handles quoting, pipes, and redirects.

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
}

pub fn tokenize(input: &str) -> TokenizeResult {
    let mut tokens = Vec::new();
    let mut current_word = String::new();
    let mut quote_state = QuoteState::None;
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
                    flush_word(&mut current_word, &mut tokens);
                    if chars.peek() == Some(&'&') {
                        chars.next();
                        tokens.push(Token::And);
                    } else {
                        tokens.push(Token::Background);
                    }
                } else if ch == ';' {
                    chars.next();
                    flush_word(&mut current_word, &mut tokens);
                    tokens.push(Token::Semicolon);
                } else if ch == '>' {
                    chars.next();
                    flush_word(&mut current_word, &mut tokens);
                    if chars.peek() == Some(&'>') {
                        chars.next();
                        tokens.push(Token::RedirectAppend);
                    } else {
                        tokens.push(Token::RedirectOut);
                    }
                } else if ch == '<' {
                    chars.next();
                    flush_word(&mut current_word, &mut tokens);
                    tokens.push(Token::RedirectIn);
                } else if ch.is_ascii_whitespace() {
                    chars.next();
                    flush_word(&mut current_word, &mut tokens);
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
    }
}

fn flush_word(word: &mut String, tokens: &mut Vec<Token>) {
    if !word.is_empty() {
        tokens.push(Token::Word(std::mem::take(word)));
    }
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
}
