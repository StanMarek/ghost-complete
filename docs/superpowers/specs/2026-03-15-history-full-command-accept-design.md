# History Full-Command Accept & Buffer-Wide Matching

**Date:** 2026-03-15
**Version target:** v0.2.1

## Problem

When accepting a history entry from the popup, only the base command (first word) is inserted instead of the full history command. For example, selecting `tmux source ~/.config/tmux/tmux.conf` inserts just `tmux`. Additionally, history entries only appear at command position (`word_index == 0`), so typing `tmux s` doesn't surface matching history.

## Approach

**Approach A — Full-text history with separate ranking pass.** History stores the full command in `text`, matching is done in a separate fuzzy pass using the full buffer as query, and acceptance deletes the entire buffer before inserting the full command.

## Design

### 1. History Provider (`gc-suggest/src/history.rs`)

- Remove the `word_index == 0` guard — history is available at any word position.
- Change `text` from first-word-only to the **full command string**.
- Set `description` to `None` — no point duplicating the full command in both fields.

```rust
// Before
let cmd_name = entry.split_whitespace().next().unwrap_or(entry);
Suggestion {
    text: cmd_name.to_string(),
    description: Some(entry.clone()),
    ...
}

// After
Suggestion {
    text: entry.clone(),
    description: None,
    ...
}
```

### 2. Engine (`gc-suggest/src/engine.rs`)

- Add a `buffer: &str` parameter to `suggest_sync`.
- Remove history from the `word_index == 0` block.
- After the existing suggestion logic (commands/specs/filesystem), run a **separate history pass**:
  1. Call `history_provider.provide()` (no longer gated by word_index).
  2. Fuzzy-rank history candidates against the **full buffer** (not `current_word`).
  3. Append ranked history results to the main results.
- Each pass applies its own `max_results` cap independently.
- `fuzzy::rank` is called twice: once for main candidates with `current_word`, once for history with the full buffer. No changes to `fuzzy::rank` itself.

```
suggest_sync(ctx, cwd, buffer):
  1. candidates = existing logic (commands at word_index==0, specs, filesystem)
  2. results = fuzzy::rank(ctx.current_word, candidates, max_results)
  3. if history enabled:
       hist_candidates = history_provider.provide(ctx, cwd)
       hist_results = fuzzy::rank(buffer, hist_candidates, max_results)
       results.extend(hist_results)
  4. return results
```

**Performance:** One extra `fuzzy::rank` call per trigger. nucleo handles 10k candidates in <1ms even with longer queries. Total `suggest_sync` stays well under 5ms.

### 3. Accept Logic (`gc-pty/src/handler.rs`)

- `accept_suggestion` checks `selected.kind == SuggestionKind::History`:
  - **History path:** Get the full buffer from the parser. Send `buffer.chars().count()` backspaces (0x7F), then type `selected.text` (the full command).
  - **Non-history path:** Unchanged — delete `current_word` chars, type `selected.text`.
- `accept_with_chaining` (directory Tab-chaining): history entries skip chaining entirely — plain accept + dismiss.
- Accept & Enter variant: works identically. Accept replaces buffer, `\r` appended to execute.

### 4. Display (`gc-overlay/src/render.rs`)

- No code changes needed. `format_item` already handles `None` descriptions gracefully.
- History entries display as: `H tmux source ~/.config/tmux/tmux.conf` (full command, no redundant description).
- Fuzzy match highlighting works automatically — nucleo matches against `text` (the full command).
- Long entries truncated by existing `max_text_chars` logic.

### 5. Callers of `suggest_sync`

All call sites in `handler.rs` that invoke `suggest_sync` must pass the buffer string. These callers already have access to the raw buffer from the parser state, so threading it through is trivial:
- `trigger_suggestions` — has `buffer` from parser
- `accept_with_chaining` — has `predicted` buffer

## Files Modified

| File | Change |
|------|--------|
| `crates/gc-suggest/src/history.rs` | Full command in `text`, remove `word_index` gate |
| `crates/gc-suggest/src/engine.rs` | Add `buffer` param to `suggest_sync`, separate history pass |
| `crates/gc-pty/src/handler.rs` | History-aware accept (full buffer delete), pass buffer to engine |
| Tests in all three files | Update assertions for new behavior |

## What Does NOT Change

- `fuzzy::rank` — no changes
- `gc-overlay` rendering — no changes
- `SuggestionKind::History` sort priority (8, always last) — no changes
- `suggest_dynamic` — history is sync-only, no async changes
- Config / keybindings / themes — no changes
