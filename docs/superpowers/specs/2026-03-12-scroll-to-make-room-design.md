# Scroll-to-Make-Room Popup Rendering

**Date:** 2026-03-12
**Status:** Approved
**Scope:** gc-overlay, gc-pty (minor)

## Problem

When the popup renders above the cursor line (not enough space below), `clear_popup()` overwrites the occupied rows with spaces. This permanently destroys terminal output in those rows — both visually and in scrollback. The damage persists after popup dismissal and through tab-cycling between completions.

Root cause: there is no mechanism to save/restore the original screen content under the popup, and the cleanup code assumes the popup area was empty.

## Solution

Never render above. When there isn't enough room below the cursor, scroll the terminal viewport up by emitting newlines to create blank space at the bottom. Then render the popup in that blank space. Cleanup (overwriting with spaces) is always safe because the popup area is empty space we created.

This is the standard approach used by fzf, zsh's native completion, and other terminal popup systems.

## Design

### Scroll Deficit Calculation

In `render_popup()`, before computing layout:

```
space_below = screen_rows - cursor_row - 1
popup_height = min(suggestions.len(), max_visible) as u16
deficit = max(0, popup_height - space_below)
```

When `deficit > 0`: scroll the viewport, adjust cursor position, then render below.
When `deficit == 0`: no change from current behavior.

### Corrected Scroll Sequence

Newlines only cause viewport scrolling when the cursor is on the last row of the screen. Emitting `\n` from the middle of the screen just moves the cursor down without scrolling. The scroll sequence must therefore:

1. Move cursor to the last viewport row first
2. Emit newlines from there (guaranteeing each one scrolls)
3. Reposition cursor to the adjusted prompt location
4. Save cursor AFTER repositioning (so DECRC restores to the correct position)

The complete sequence:

```
1. \x1b[?2026h              — begin synchronized output
2. \x1b[{screen_rows};1H    — CUP: move cursor to last viewport row
3. \n x deficit              — emit deficit newlines (each scrolls the viewport by 1)
4. \x1b[{adj_row+1};{col+1}H — CUP: move cursor to adjusted prompt position
5. \x1b7                     — DECSC: save cursor (now at the correct post-scroll position)
6. [render popup rows below] — normal popup rendering at start_row = adj_row + 1
7. \x1b8                     — DECRC: restore cursor to saved position (prompt line)
8. \x1b[?2026l              — end synchronized output
```

Where `adj_row = cursor_row - deficit`.

**Small terminal guard:** If `screen_rows <= popup_height`, cap `popup_height` to `screen_rows - 1` before computing deficit. This guarantees at least the prompt row remains visible and prevents `adj_row` from underflowing (since `deficit` can never exceed `cursor_row` when `popup_height < screen_rows`).

**Why this order:** DECSC saves a viewport-relative row. If we saved before scrolling, DECRC would restore to the pre-scroll row number, which now points into the popup area — not the prompt. By saving AFTER the scroll + CUP repositioning, DECSC captures the correct post-scroll prompt position.

**Why CUP to last row first:** Newlines from the middle of the screen move the cursor down without scrolling. We need guaranteed scrolls, so we position at the bottom row where each `\n` triggers a viewport scroll.

### Re-render Drift Prevention

When the user tab-cycles between completions, the handler calls `clear_popup()` then `render_popup()` in succession. Without tracking scroll state, each render would recalculate deficit from the parser's `cursor_row` (which doesn't know about prior scrolls, since render output bypasses the parser) and scroll again, drifting the viewport further down.

**Fix:** `render_popup()` returns `PopupLayout` which now includes a `scroll_deficit: u16` field. The handler stores this in `last_layout`. On re-render:

- Before re-rendering, the handler passes the previous deficit to `render_popup()` as a new `prior_deficit: u16` parameter
- `render_popup()` uses `adjusted_cursor_row = cursor_row - prior_deficit` for both the space calculation and layout computation
- If the new popup needs MORE deficit (e.g., more items), only the incremental difference is scrolled
- If the new popup needs LESS or equal deficit, no additional scrolling occurs

This ensures tab-cycling never accumulates viewport drift.

### Parser State Isolation

The render buffer is written directly to stdout by the handler (`stdout.write_all(&render_buf)`), bypassing gc-parser entirely. This means:

- The parser's `TerminalState.cursor_row` is unaffected by scroll newlines — it still reflects the shell's cursor position
- The parser's `saved_cursor` field is unaffected by DECSC/DECRC in the render buffer
- This is correct behavior: the parser tracks shell state, the overlay manages its own rendering state independently

### File Changes

#### `crates/gc-overlay/src/render.rs`

- `render_popup()`: Add `prior_deficit: u16` parameter. Calculate deficit accounting for prior scroll. If net new scrolling needed, emit the corrected scroll sequence (CUP to bottom, newlines, CUP to adjusted position) before DECSC. Pass `adjusted_cursor_row` to `compute_layout()`.
- `clear_popup()`: No changes — it writes spaces to the popup area, which is always below (safe).

#### `crates/gc-overlay/src/layout.rs`

- `compute_layout()`: Remove the above-rendering branch. Layout always computes `start_row = cursor_row + 1`. The caller is responsible for passing an adjusted cursor_row that accounts for scrolling.

#### `crates/gc-overlay/src/types.rs`

- `PopupLayout`: Remove `renders_above` field. Add `scroll_deficit: u16` field.

#### `crates/gc-pty/src/handler.rs`

- `render()` / `render_at()`: Pass `prior_deficit` from `last_layout` (0 if no prior layout). Store returned layout's `scroll_deficit` for next render cycle.
- `dismiss()`: After `clear_popup()`, reset stored deficit to 0.
- `handle_resize()`: Change from calling `render()` to calling `dismiss()`. After a resize, screen dimensions change and prior deficit is stale. Dismissing resets deficit to 0; the popup will be recomputed with fresh dimensions on the next trigger.

### Edge Cases

| Case | Behavior |
|------|----------|
| Cursor on last row, popup height 10 | Deficit = 10. CUP to last row is a no-op (already there). 10 newlines scroll viewport up 10 rows. |
| Cursor on row 0, screen_rows = 24 | space_below = 23. Deficit only if popup_height > 23. Unlikely with max_visible = 10. |
| popup_height > screen_rows | Capped to `screen_rows - 1` before deficit calc. Guarantees prompt row stays visible, prevents `adj_row` underflow. |
| Very small terminal (e.g., 10 rows) | popup_height capped to 9. Deficit = max(0, 9 - space_below). adj_row >= 0 guaranteed. |
| Tab cycling (dismiss + re-render) | prior_deficit carried forward. No additional scrolling unless new popup is taller. |
| Popup with plenty of room below | Deficit = 0. Zero scroll. Identical to current behavior. |
| Terminal resize during popup | `handle_resize()` must call `dismiss()` (not `render()`), resetting deficit to 0. Popup recomputed on next trigger with fresh screen dimensions. This is a required code change — current code calls `render()` which would use stale deficit with new screen_rows. |
| Dismiss after scroll | Cleanup spaces the below area. Scrolled-up content is safely in scrollback. Deficit resets to 0. |

### What Gets Removed

- The `renders_above` branch in `compute_layout()` (layout.rs line 32-37)
- The `renders_above` field on `PopupLayout` (types.rs)
- Any test assertions that check for above-rendering behavior

### Testing

- Update existing layout tests that assert `renders_above == true` — layout now always places below
- Add test: deficit calculation when cursor near bottom (deficit > 0)
- Add test: deficit = 0 when plenty of room below
- Add test: scroll sequence emits CUP-to-bottom + newlines + CUP-to-adjusted when deficit > 0
- Add test: DECSC occurs AFTER scroll repositioning (verify byte order in buffer)
- Add test: prior_deficit prevents re-scrolling on tab cycle
- Add test: incremental deficit (new popup taller than previous — only scrolls the difference)
- Add test: small terminal (screen_rows <= max_visible) caps popup_height to screen_rows - 1
- Add test: adj_row never underflows (deficit <= cursor_row when popup_height < screen_rows)
- Existing clear_popup tests remain valid (cleanup logic unchanged)
- Integration test: verify popup fits below after scroll sequence (end-to-end correctness)

## Performance

No impact. The scroll adds at most one CUP (~8 bytes) + `deficit` newline bytes + one CUP (~8 bytes) to the output buffer. This is negligible compared to the popup rendering itself.

## Alternatives Considered

**Screen content buffer (save/restore):** Track all terminal output per-row in gc-parser, save rows before above-render, restore on cleanup. Rejected: massive complexity, fights the parser-only architecture (vte choice over alacritty_terminal).

**Adaptive height (cap to space_below):** Never render above, show fewer items. Rejected: degraded UX when cursor near bottom (0-2 visible items). Doesn't solve the problem, just avoids it.
