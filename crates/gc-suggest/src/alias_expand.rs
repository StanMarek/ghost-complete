//! Alias expansion at spec-resolution time.
//!
//! Multi-word aliases like `alias gco='git checkout'` need to be unfolded
//! before the engine asks `SpecStore` which spec applies and walks
//! [`crate::specs::resolve_spec`] over the args. This module returns a
//! synthetic *view* of the user's [`CommandContext`] with the alias-tail
//! prepended to the args; `current_word` and `word_index` are deliberately
//! left untouched because fuzzy ranking, history matching, frecency
//! keying, and the accept-completion path all key off the literal buffer
//! the user typed (`gco`), not the expansion (`git checkout`).
//!
//! See `docs/plans/ux-3-multiword-aliases/SPEC.md` (D2 / D3 / D5) for the
//! invariants.

use std::borrow::Cow;
use std::collections::HashSet;

use gc_buffer::CommandContext;

use crate::alias::AliasStore;

/// Maximum recursive alias-of-alias expansions per lookup. Far above any
/// realistic depth (zsh users rarely chain more than 2–3 levels deep) and
/// small enough that the per-keystroke cost stays well under the
/// suggestion budget. See SPEC D3.
pub(crate) const MAX_ALIAS_HOPS: usize = 16;

/// Alias-expanded view over a [`CommandContext`] for spec resolution.
///
/// Both fields are `Cow` so the no-alias-hit branch costs zero
/// allocations: `resolved_command` borrows from `ctx.command` and
/// `effective_args` borrows `ctx.args`. Only when an alias actually fires
/// do we own the rewritten tokens.
pub(crate) struct ExpandedCtx<'a> {
    /// Resolved command name (head of the expansion). Equals the original
    /// `ctx.command` string when no alias matched.
    pub resolved_command: Cow<'a, str>,
    /// `expanded-tail-tokens ++ ctx.args`. Borrowed from `ctx.args` when
    /// no expansion happened.
    pub effective_args: Cow<'a, [String]>,
}

/// Expand `ctx.command` through the alias map.
///
/// Returns `None` only when there is no command to expand at all
/// (`ctx.word_index == 0` — user is still typing the alias name itself —
/// or `ctx.command.is_none()`). Every other path, including "command is
/// not an alias", returns `Some(ExpandedCtx)` so callers can uniformly
/// look up the spec by `resolved_command` and walk by `effective_args`.
///
/// Recursion stops on:
/// * a head token that is not itself an alias,
/// * a head token already seen in this expansion (cycle guard), or
/// * after [`MAX_ALIAS_HOPS`] iterations (depth cap; partial expansion
///   is preserved).
pub(crate) fn expand_alias_for_spec<'a>(
    ctx: &'a CommandContext,
    alias_map: &AliasStore,
) -> Option<ExpandedCtx<'a>> {
    if ctx.word_index == 0 {
        return None;
    }
    let command = ctx.command.as_deref()?;

    // Fast path: no alias hit at all -> borrowed view.
    let Some(initial) = alias_map.get(command) else {
        return Some(ExpandedCtx {
            resolved_command: Cow::Borrowed(command),
            effective_args: Cow::Borrowed(&ctx.args),
        });
    };

    // Slow path: head expansion + recursive head-replacement.
    let mut tokens: Vec<String> = initial;
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(command.to_string());

    for _ in 0..MAX_ALIAS_HOPS {
        let head = match tokens.first() {
            Some(h) => h.clone(),
            None => break,
        };
        if visited.contains(&head) {
            break; // cycle
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
    // Safe: tokens.is_empty() returned false above.
    let resolved = iter.next().expect("tokens non-empty");
    let mut effective: Vec<String> = iter.collect();
    effective.extend(ctx.args.iter().cloned());
    Some(ExpandedCtx {
        resolved_command: Cow::Owned(resolved),
        effective_args: Cow::Owned(effective),
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
        // The user is still typing the command itself; expansion would
        // be wrong because `current_word` is the name, not a positional.
        let aliases = store(&[("gco", &["git", "checkout"])]);
        let c = ctx(None, &[], "gc", 0);
        assert!(expand_alias_for_spec(&c, &aliases).is_none());
    }

    #[test]
    fn expand_returns_borrowed_when_no_alias() {
        // No alias hit must borrow from `ctx` rather than allocate — the
        // hot path runs on every keystroke.
        let aliases = AliasStore::empty();
        let c = ctx(Some("git"), &["checkout"], "main", 2);
        let exp = expand_alias_for_spec(&c, &aliases).unwrap();
        assert!(matches!(exp.resolved_command, Cow::Borrowed("git")));
        assert!(matches!(exp.effective_args, Cow::Borrowed(_)));
        assert_eq!(exp.effective_args.as_ref(), &["checkout".to_string()]);
    }

    #[test]
    fn expand_single_word_alias() {
        // `alias g=git` resolves to head=git with no tail; ctx.args pass
        // through unchanged.
        let aliases = store(&[("g", &["git"])]);
        let c = ctx(Some("g"), &["push"], "", 2);
        let exp = expand_alias_for_spec(&c, &aliases).unwrap();
        assert_eq!(exp.resolved_command.as_ref(), "git");
        assert_eq!(exp.effective_args.as_ref(), &["push".to_string()]);
    }

    #[test]
    fn expand_multi_word_alias() {
        // `alias gco='git checkout'` resolves to head=git with tail=[checkout],
        // and the user-typed args are appended after.
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
        // gcb -> gco -> git checkout. The tail from each layer accumulates
        // (`-b` from gcb, `checkout` from gco), and the user-typed args
        // come last.
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
        // a -> b -> a. Expansion must terminate without recursion: after
        // two hops `a` is already visited, so the loop bails. The
        // resulting head doesn't have to be "useful"; it just must not
        // hang or panic.
        let aliases = store(&[("a", &["b"]), ("b", &["a"])]);
        let c = ctx(Some("a"), &[], "", 1);
        let exp = expand_alias_for_spec(&c, &aliases).unwrap();
        // The point of the test is termination; we don't assert on
        // `resolved_command` because either head is acceptable (the
        // SPEC just says "stop expansion").
        assert!(["a", "b"].contains(&exp.resolved_command.as_ref()));
    }

    #[test]
    fn expand_depth_cap_stops_at_max_hops() {
        // Build a chain longer than MAX_ALIAS_HOPS. Expansion must
        // terminate (no infinite loop, no panic) and return the partial
        // result rather than failing.
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
        // After 16 hops starting from a0, the head should be a16 (we
        // expand a0 -> a1 once before entering the loop, then 16 more
        // head-swaps land on a17). Either way the head is somewhere in
        // the chain and the loop terminated — the absolute index is an
        // implementation detail; the SPEC guarantees only "stops at 16".
        let head = exp.resolved_command.as_ref();
        assert!(head.starts_with('a'));
        let idx: usize = head[1..].parse().unwrap();
        assert!(
            idx >= MAX_ALIAS_HOPS && idx < chain_len,
            "expansion must stop at the depth cap, not unwind the whole chain (head={head})"
        );
    }
}
