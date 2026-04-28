//! Alias-expanded view over CommandContext for spec resolution.

use std::borrow::Cow;
use std::collections::HashSet;

use gc_buffer::CommandContext;

use crate::alias::AliasStore;

/// Recursion cap for chained alias-of-alias expansion (cycle/depth guard).
pub(crate) const MAX_ALIAS_HOPS: usize = 16;

/// Cow-borrowed in the no-alias-hit path to avoid per-keystroke allocations.
pub(crate) struct ExpandedCtx<'a> {
    pub resolved_command: Cow<'a, str>,
    pub effective_args: Cow<'a, [String]>,
    pub aliased: bool,
}

/// Walk ctx.command through the alias map; cycle-guarded, capped at MAX_ALIAS_HOPS.
pub(crate) fn expand_alias_for_spec<'a>(
    ctx: &'a CommandContext,
    alias_map: &AliasStore,
) -> Option<ExpandedCtx<'a>> {
    if ctx.word_index == 0 {
        return None;
    }
    let command = ctx.command.as_deref()?;

    let Some(initial) = alias_map.get(command) else {
        return Some(ExpandedCtx {
            resolved_command: Cow::Borrowed(command),
            effective_args: Cow::Borrowed(&ctx.args),
            aliased: false,
        });
    };

    let mut tokens: Vec<String> = initial;
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(command.to_string());

    for _ in 0..MAX_ALIAS_HOPS {
        let head = match tokens.first() {
            Some(h) => h.clone(),
            None => break,
        };
        if visited.contains(&head) {
            break;
        }
        match alias_map.get(&head) {
            Some(next) => {
                visited.insert(head);
                let tail: Vec<String> = tokens.drain(1..).collect();
                tokens = next;
                tokens.extend(tail);
            }
            None => break,
        }
    }

    if tokens.is_empty() {
        return None;
    }
    let mut iter = tokens.into_iter();
    let resolved = iter.next().expect("tokens non-empty");
    let mut effective: Vec<String> = iter.collect();
    effective.extend(ctx.args.iter().cloned());
    Some(ExpandedCtx {
        resolved_command: Cow::Owned(resolved),
        effective_args: Cow::Owned(effective),
        aliased: true,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use gc_buffer::{CommandContext, QuoteState};

    use super::*;
    use crate::alias::AliasStore;

    fn ctx(
        command: Option<&str>,
        args: &[&str],
        current_word: &str,
        word_index: usize,
    ) -> CommandContext {
        CommandContext {
            command: command.map(String::from),
            args: args.iter().map(|s| (*s).to_string()).collect(),
            current_word: current_word.to_string(),
            word_index,
            is_flag: current_word.starts_with('-'),
            is_long_flag: current_word.starts_with("--"),
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: QuoteState::None,
            is_first_segment: true,
        }
    }

    fn store(entries: &[(&str, &[&str])]) -> AliasStore {
        let store = AliasStore::empty();
        let map: HashMap<String, Vec<String>> = entries
            .iter()
            .map(|(name, toks)| {
                let v: Vec<String> = toks.iter().map(|s| (*s).to_string()).collect();
                ((*name).to_string(), v)
            })
            .collect();
        store.populate(map);
        store
    }

    #[test]
    fn expand_returns_none_at_word_index_zero() {
        // word_index 0 means current_word is the alias name itself, not a positional.
        let aliases = store(&[("gco", &["git", "checkout"])]);
        let c = ctx(None, &[], "gc", 0);
        assert!(expand_alias_for_spec(&c, &aliases).is_none());
    }

    #[test]
    fn expand_returns_borrowed_when_no_alias() {
        let aliases = AliasStore::empty();
        let c = ctx(Some("git"), &["checkout"], "main", 2);
        let exp = expand_alias_for_spec(&c, &aliases).unwrap();
        assert!(matches!(exp.resolved_command, Cow::Borrowed("git")));
        assert!(matches!(exp.effective_args, Cow::Borrowed(_)));
        assert!(!exp.aliased);
        assert_eq!(exp.effective_args.as_ref(), &["checkout".to_string()]);
    }

    #[test]
    fn expand_single_word_alias() {
        let aliases = store(&[("g", &["git"])]);
        let c = ctx(Some("g"), &["push"], "", 2);
        let exp = expand_alias_for_spec(&c, &aliases).unwrap();
        assert_eq!(exp.resolved_command.as_ref(), "git");
        assert_eq!(exp.effective_args.as_ref(), &["push".to_string()]);
    }

    #[test]
    fn expand_multi_word_alias() {
        let aliases = store(&[("gco", &["git", "checkout"])]);
        let c = ctx(Some("gco"), &["main"], "", 2);
        let exp = expand_alias_for_spec(&c, &aliases).unwrap();
        assert_eq!(exp.resolved_command.as_ref(), "git");
        assert_eq!(
            exp.effective_args.as_ref(),
            &["checkout".to_string(), "main".to_string()]
        );
    }

    #[test]
    fn expand_chained_aliases() {
        let aliases = store(&[("gcb", &["gco", "-b"]), ("gco", &["git", "checkout"])]);
        let c = ctx(Some("gcb"), &["feature"], "", 2);
        let exp = expand_alias_for_spec(&c, &aliases).unwrap();
        assert_eq!(exp.resolved_command.as_ref(), "git");
        assert_eq!(
            exp.effective_args.as_ref(),
            &[
                "checkout".to_string(),
                "-b".to_string(),
                "feature".to_string(),
            ]
        );
    }

    #[test]
    fn expand_cycle_guard() {
        // a -> b -> a must terminate, not stack-overflow.
        let aliases = store(&[("a", &["b"]), ("b", &["a"])]);
        let c = ctx(Some("a"), &[], "", 1);
        let exp = expand_alias_for_spec(&c, &aliases).unwrap();
        assert!(["a", "b"].contains(&exp.resolved_command.as_ref()));
    }

    #[test]
    fn expand_depth_cap_stops_at_max_hops() {
        let chain_len = MAX_ALIAS_HOPS + 5;
        let names: Vec<String> = (0..chain_len).map(|i| format!("a{i}")).collect();
        let mut entries: Vec<(&str, Vec<String>)> = Vec::new();
        for i in 0..chain_len - 1 {
            entries.push((names[i].as_str(), vec![names[i + 1].clone()]));
        }
        let store = AliasStore::empty();
        let map: HashMap<String, Vec<String>> = entries
            .into_iter()
            .map(|(n, v)| (n.to_string(), v))
            .collect();
        store.populate(map);

        let c = ctx(Some("a0"), &[], "", 1);
        let exp = expand_alias_for_spec(&c, &store).unwrap();
        let head = exp.resolved_command.as_ref();
        assert!(head.starts_with('a'));
        let idx: usize = head[1..].parse().unwrap();
        assert!(
            idx >= MAX_ALIAS_HOPS && idx < chain_len,
            "expansion must stop at the depth cap, not unwind the whole chain (head={head})"
        );
    }
}
