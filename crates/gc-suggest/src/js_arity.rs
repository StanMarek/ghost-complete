//! Load-time arity probe for Fig `postProcess` bodies.
//!
//! Fig calls `postProcess(out, tokens)`. Most corpus bodies declare only
//! `out`, and their result is a pure function of the script's stdout, so
//! it can be cached across keystrokes. Bodies that declare `tokens` can
//! change with every keystroke. The engine and the PTY handler need to
//! tell the two apart, and the flag is derived once, when a spec loads,
//! from the parameter list alone.
//!
//! The probe is a small scanner, not a JS parser. It recognises the
//! function-literal shapes the converter emits (`x => ..`, `(a, b) => ..`,
//! `function name(a, b) {..}`, each optionally `async`) and counts
//! top-level commas in the parameter list, skipping nested brackets and
//! string literals. Anything else is reported as unrecognised, and
//! [`post_process_reads_tokens`] treats unrecognised sources as reading
//! tokens: a wasted cache slot is cheap, a stale popup is a bug.

/// True when a `post_process` body can observe Fig's `tokens` argument:
/// it declares a second formal parameter, mentions `arguments`, or has a
/// shape [`formal_param_count`] doesn't recognise.
pub(crate) fn post_process_reads_tokens(source: &str) -> bool {
    match formal_param_count(source) {
        Some(count) => count >= 2 || mentions_identifier(source, "arguments"),
        None => true,
    }
}

/// Count the formal parameters of a JS function-literal source. A rest
/// parameter, destructuring pattern, or defaulted parameter counts as
/// one. Returns `None` when `source` does not open with a function
/// literal this probe recognises.
fn formal_param_count(source: &str) -> Option<usize> {
    let s = source.trim_start();
    let s = strip_keyword(s, "async").map_or(s, str::trim_start);

    if let Some(rest) = strip_keyword(s, "function") {
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('*').unwrap_or(rest).trim_start();
        let rest = rest[identifier_len(rest)..].trim_start();
        let (count, after) = param_list(rest)?;
        return after.trim_start().starts_with('{').then_some(count);
    }

    if s.starts_with('(') {
        let (count, after) = param_list(s)?;
        return after.trim_start().starts_with("=>").then_some(count);
    }

    let ident_len = identifier_len(s);
    if ident_len > 0 && s[ident_len..].trim_start().starts_with("=>") {
        return Some(1);
    }
    None
}

/// `s` minus a leading `keyword`, if the keyword is a whole word.
fn strip_keyword<'a>(s: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = s.strip_prefix(keyword)?;
    match rest.chars().next() {
        Some(c) if is_identifier_char(c) => None,
        _ => Some(rest),
    }
}

fn is_identifier_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

fn identifier_len(s: &str) -> usize {
    s.char_indices()
        .find(|&(_, c)| !is_identifier_char(c))
        .map_or(s.len(), |(idx, _)| idx)
}

/// Scan a parenthesised parameter list at the start of `s`. Returns the
/// number of non-empty top-level segments and the text after the closing
/// parenthesis, or `None` if `s` doesn't start with `(` or the list is
/// unterminated.
fn param_list(s: &str) -> Option<(usize, &str)> {
    let mut chars = s.char_indices();
    if chars.next()?.1 != '(' {
        return None;
    }
    let mut depth = 0usize;
    let mut count = 0usize;
    let mut segment_has_content = false;
    while let Some((idx, c)) = chars.next() {
        match c {
            '"' | '\'' | '`' => {
                skip_string(&mut chars, c)?;
                segment_has_content = true;
            }
            '(' | '[' | '{' => {
                depth += 1;
                segment_has_content = true;
            }
            ')' if depth == 0 => {
                count += usize::from(segment_has_content);
                return Some((count, &s[idx + 1..]));
            }
            ')' | ']' | '}' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => {
                count += usize::from(segment_has_content);
                segment_has_content = false;
            }
            c if c.is_whitespace() => {}
            _ => segment_has_content = true,
        }
    }
    None
}

/// Advance `chars` past the closing `quote` of a string literal whose
/// opening quote was just consumed.
fn skip_string(chars: &mut std::str::CharIndices<'_>, quote: char) -> Option<()> {
    while let Some((_, c)) = chars.next() {
        if c == '\\' {
            chars.next()?;
        } else if c == quote {
            return Some(());
        }
    }
    None
}

/// Whole-word search for `ident`. Occurrences inside strings count too,
/// which errs toward treating the body as token-dependent.
fn mentions_identifier(source: &str, ident: &str) -> bool {
    source.match_indices(ident).any(|(idx, _)| {
        let before = source[..idx].chars().next_back();
        let after = source[idx + ident.len()..].chars().next();
        !before.is_some_and(|c| is_identifier_char(c) || c == '.')
            && !after.is_some_and(is_identifier_char)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_single_param_arrow_shapes() {
        assert_eq!(formal_param_count("out => out.split('\\n')"), Some(1));
        assert_eq!(formal_param_count("e=>e"), Some(1));
        assert_eq!(formal_param_count("async e => e"), Some(1));
        assert_eq!(formal_param_count("(e) => e"), Some(1));
    }

    #[test]
    fn counts_multi_param_arrow_shapes() {
        assert_eq!(formal_param_count("(e,t)=>{let i=D(e)}"), Some(2));
        assert_eq!(formal_param_count("async (e, t) => t"), Some(2));
        assert_eq!(formal_param_count("(e, ...rest) => rest"), Some(2));
        assert_eq!(formal_param_count("(e,t,)=>t"), Some(2));
    }

    #[test]
    fn counts_function_expression_shapes() {
        assert_eq!(
            formal_param_count("function(t){return l(t,\"Roles\")}"),
            Some(1)
        );
        assert_eq!(formal_param_count("function(e,[n]){return n}"), Some(2));
        assert_eq!(formal_param_count("function(e,n=[]){return n}"), Some(2));
        assert_eq!(formal_param_count("function named(e){return e}"), Some(1));
        assert_eq!(formal_param_count("async function(e,t){return t}"), Some(2));
        assert_eq!(formal_param_count("function*(e){yield e}"), Some(1));
    }

    #[test]
    fn counts_zero_param_shapes() {
        assert_eq!(formal_param_count("function(){return[]}"), Some(0));
        assert_eq!(formal_param_count("() => []"), Some(0));
        assert_eq!(formal_param_count("function( ){return[]}"), Some(0));
    }

    #[test]
    fn ignores_commas_nested_in_patterns_defaults_and_strings() {
        assert_eq!(formal_param_count("function(e,{a,b}){}"), Some(2));
        assert_eq!(formal_param_count("function(e=[1,2]){}"), Some(1));
        assert_eq!(formal_param_count("function(e=f(1,2)){}"), Some(1));
        assert_eq!(formal_param_count("function(e=\",)\"){}"), Some(1));
        assert_eq!(formal_param_count("function(e=',)'){}"), Some(1));
        assert_eq!(formal_param_count("function(e=`,)`){}"), Some(1));
        assert_eq!(formal_param_count("function(e=\"\\\",\"){}"), Some(1));
    }

    #[test]
    fn rejects_sources_that_are_not_function_literals() {
        assert_eq!(formal_param_count(""), None);
        assert_eq!(formal_param_count("tokens.map(t => t)"), None);
        assert_eq!(formal_param_count("postProcess(e){return[]}"), None);
        assert_eq!(formal_param_count("(function(e,t){})"), None);
        assert_eq!(formal_param_count("function(e,t"), None);
    }

    #[test]
    fn rejects_lists_that_close_somewhere_other_than_a_body() {
        // A `)` the scanner can't see into (here, inside a regex literal)
        // ends the list early. Requiring `{` / `=>` right after it turns
        // that into "unrecognised" instead of an undercount.
        assert_eq!(formal_param_count("function(e=/)/,t){return t}"), None);
        assert_eq!(formal_param_count("(e=/)/,t)=>t"), None);
        assert!(post_process_reads_tokens("function(e=/)/,t){return t}"));
    }

    #[test]
    fn reads_tokens_when_second_param_declared() {
        assert!(post_process_reads_tokens("function(e,[n]){return n}"));
        assert!(post_process_reads_tokens("function(e,n=[]){return n}"));
        assert!(post_process_reads_tokens("(r,e)=>e[1]"));
    }

    #[test]
    fn single_param_bodies_do_not_read_tokens() {
        assert!(!post_process_reads_tokens("out => out.split('\\n')"));
        assert!(!post_process_reads_tokens(
            "function(t){return l(t,\"Roles\")}"
        ));
        assert!(!post_process_reads_tokens("function(){return[]}"));
    }

    #[test]
    fn arguments_object_counts_as_reading_tokens() {
        assert!(post_process_reads_tokens(
            "function(e){return arguments[1]}"
        ));
        // Only the bare identifier counts, not a longer name containing it.
        assert!(!post_process_reads_tokens(
            "function(e){return e.myarguments}"
        ));
    }

    #[test]
    fn unrecognised_sources_are_treated_as_reading_tokens() {
        // Can't prove the body ignores tokens, so assume it doesn't.
        assert!(post_process_reads_tokens("postProcess(e){return[]}"));
        assert!(post_process_reads_tokens(""));
    }
}
