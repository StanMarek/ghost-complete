# History Full-Command Accept & Buffer-Wide Matching

**Date:** 2026-03-15
**Version target:** v0.2.1

## Problem

When accepting a history entry from the popup, only the base command (first word) is inserted instead of the full history command. For example, selecting `tmux source ~/.config/tmux/tmux.conf` inserts just `tmux`. Additionally, history entries only appear at command position (`word_index == 0`), so typing `tmux s` doesn't surface matching history.

## Approach

**Approach A — Full-text history with separate ranking pass.** History stores the full command in `text`, matching is done in a separate fuzzy pass using the full buffer as query, and acceptance deletes the entire buffer before inserting the full command.

## Design

### 0. Buffer Context (`gc-buffer/src/context.rs`)

- Add `is_first_segment: bool` to `CommandContext`. Computed as `segment_start == 0` — true when there are no preceding `|`, `&&`, `||`, or `;` operators.
- `segment_start` is already computed (line 43) but not exposed. This makes it available for gating history.

```rust
// In CommandContext struct
pub is_first_segment: bool,

// In parse_command_context, at the end
CommandContext {
    ...
    is_first_segment: segment_start == 0,
}
```

### 1. History Provider (`gc-suggest/src/history.rs`)

- Replace the `word_index == 0` guard with `is_first_segment` guard — history only makes sense when the user is typing a standalone command, not inside a pipe, chain, or redirect.
- Change `text` from first-word-only to the **full command string**.
- Set `description` to `None` — no point duplicating the full command in both fields.

```rust
// Before
if ctx.word_index != 0 { return Ok(Vec::new()); }
let cmd_name = entry.split_whitespace().next().unwrap_or(entry);
Suggestion {
    text: cmd_name.to_string(),
    description: Some(entry.clone()),
    ...
}

// After
if !ctx.is_first_segment { return Ok(Vec::new()); }
Suggestion {
    text: entry.clone(),
    description: None,
    ...
}
```

### 2. Engine (`gc-suggest/src/engine.rs`)

- Add a `buffer: &str` parameter to `suggest_sync`.
- Remove history from the `word_index == 0` block (commands provider stays).
- After the existing suggestion logic (commands/specs/filesystem), run a **separate history pass**:
  1. Call `history_provider.provide()` (gated by `is_first_segment` inside the provider).
  2. Fuzzy-rank history candidates against the **full buffer** (not `current_word`).
  3. Append ranked history results to the main results.
- Each pass applies its own `max_results` cap independently. Combined result may exceed `max_results` (up to 2x), but popup's `max_visible` handles display; the extra memory/sorting is negligible.
- `fuzzy::rank` is called twice: once for main candidates with `current_word`, once for history with the full buffer. No changes to `fuzzy::rank` itself.

```
suggest_sync(ctx, cwd, buffer):
  1. candidates = existing logic (commands at word_index==0, specs, filesystem)
  2. results = fuzzy::rank(ctx.current_word, candidates, max_results)
  3. if history enabled:
       hist_candidates = history_provider.provide(ctx, cwd)  // returns empty if !is_first_segment
       hist_results = fuzzy::rank(buffer, hist_candidates, max_results)
       results.extend(hist_results)
  4. return results
```

**Performance:** One extra `fuzzy::rank` call per trigger. nucleo handles 10k candidates in <1ms even with longer queries. Total `suggest_sync` stays well under 5ms.

### 3. Accept Logic (`gc-pty/src/handler.rs`)

- `accept_suggestion` checks `selected.kind == SuggestionKind::History`:
  - **History path:** Get the full buffer and `buffer_cursor` (char offset) from the parser. Send `buffer_cursor` backspaces (0x7F) to delete everything before cursor, then type `selected.text` (the full command). Note: in practice the cursor is always at end-of-buffer when the popup is visible (arrow keys dismiss/forward the popup), but using `buffer_cursor` instead of `buffer.chars().count()` is defensive against edge cases.
  - **Non-history path:** Unchanged — delete `current_word` chars, type `selected.text`.
- `accept_with_chaining` (directory Tab-chaining): history entries skip chaining entirely — plain accept + dismiss. History commands aren't directory paths.
- Accept & Enter variant: works identically. Accept replaces buffer, `\r` appended to execute.

### 4. Display (`gc-overlay/src/render.rs`)

- No code changes needed. `format_item` already handles `None` descriptions gracefully.
- History entries display as: `H tmux source ~/.config/tmux/tmux.conf` (full command, no redundant description).
- Fuzzy match highlighting works automatically — nucleo matches against `text` (the full command).
- Long entries truncated by existing `max_text_chars` logic. History entries will be truncated more often than before (full commands vs first word) — this is acceptable, the important part is visible in the beginning of the entry.

### 5. Callers of `suggest_sync`

All call sites that invoke `suggest_sync` must pass the buffer string:
- `handler.rs: trigger_suggestions` — has `buffer` from parser state
- `handler.rs: accept_with_chaining` — has `predicted` buffer (the predicted buffer after directory accept, not the parser's current buffer)
- `benches/suggest_bench.rs` — benchmark call sites (3 invocations). Pass the buffer used to construct the `CommandContext` (e.g., `"gi"` for command-position benchmarks, `"git ch"` for subcommand benchmarks).

### 6. Edge Cases

**Pipes and chains (`|`, `;`, `&&`, `||`):** History is suppressed when `!ctx.is_first_segment`. The `is_first_segment` field is false after any pipe, chain, or semicolon operator. Full commands don't make sense as pipe/chain segments, and accepting one would require deleting only the current segment — which is complex and not useful. This covers all compound command cases.

**Multi-byte characters:** `buffer_cursor` is already a char offset, and backspace (0x7F) deletes by character in the shell. No special handling needed.

**Empty buffer:** History provider returns all entries, fuzzy matching with empty query returns them sorted by kind priority. Accepting replaces empty buffer with the full command. Works correctly.

**History dedup with full text:** With full commands as `text`, entries like `git push` and `git push origin main` are now distinct (previously both collapsed to `text: "git"`). This is correct — they are different commands.

## Files Modified

| File | Change |
|------|--------|
| `crates/gc-buffer/src/context.rs` | Add `is_first_segment` field to `CommandContext` |
| `crates/gc-suggest/src/history.rs` | Full command in `text`, replace `word_index` gate with `is_first_segment` gate |
| `crates/gc-suggest/src/engine.rs` | Add `buffer` param to `suggest_sync`, separate history pass |
| `crates/gc-pty/src/handler.rs` | History-aware accept (full buffer delete via `buffer_cursor`), pass buffer to engine |
| `crates/gc-suggest/benches/suggest_bench.rs` | Update `suggest_sync` call sites with buffer param |
| Tests in buffer/history/engine/handler/commands/filesystem/specs | Update assertions for new behavior, add `is_first_segment: true` to all 15+ direct `CommandContext` struct constructions (no `Default` impl) |

## What Does NOT Change

- `fuzzy::rank` — no changes
- `gc-overlay` rendering — no changes
- `SuggestionKind::History` sort priority (8, always last) — no changes
- `suggest_dynamic` — history is sync-only, no async changes
- Config / keybindings / themes — no changes
