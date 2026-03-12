# Scroll-to-Make-Room Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix above-cursor popup rendering that permanently destroys terminal scrollback by always rendering below with viewport scrolling.

**Architecture:** Remove above-rendering path from gc-overlay. When popup doesn't fit below, scroll viewport via newlines emitted from the last row, then render below. Track scroll deficit across re-renders to prevent drift during tab-cycling.

**Tech Stack:** Rust, ANSI escape sequences (CUP, DECSC/DECRC, DECSET 2026)

**Spec:** `docs/superpowers/specs/2026-03-12-scroll-to-make-room-design.md`

---

## Chunk 1: gc-overlay — Types, Layout, and Scroll Rendering

All gc-overlay changes are committed together to avoid intermediate broken states (types.rs, layout.rs, render.rs are tightly coupled).

### Task 1: Update PopupLayout, layout, and render with scroll-to-make-room

**Files:**
- Modify: `crates/gc-overlay/src/types.rs:61-68`
- Modify: `crates/gc-overlay/src/layout.rs:30-37` (logic) and `68-226` (tests)
- Modify: `crates/gc-overlay/src/render.rs:68-122` (render_popup) and `219-580` (tests)

- [ ] **Step 1: Update PopupLayout — replace `renders_above` with `scroll_deficit`**

In `types.rs`, replace the `PopupLayout` struct (lines 61-68):

```rust
#[derive(Debug, Clone)]
pub struct PopupLayout {
    pub start_row: u16,
    pub start_col: u16,
    pub width: u16,
    pub height: u16,
    pub scroll_deficit: u16,
}
```

- [ ] **Step 2: Update `compute_layout()` — always place below, remove above branch**

In `layout.rs`, replace lines 30-37 (the vertical positioning block) with:

```rust
    // Always render below cursor. Caller is responsible for adjusting
    // cursor_row via scroll deficit before calling this function.
    let start_row = cursor_row + 1;
```

Update the struct construction at lines 46-52:

```rust
    PopupLayout {
        start_row,
        start_col,
        width,
        height,
        scroll_deficit: 0, // Caller sets this after computing scroll
    }
```

- [ ] **Step 3: Update layout test `test_popup_above_cursor` — now verifies always-below**

Rename and update the test:

```rust
    #[test]
    fn test_popup_always_below_even_near_bottom() {
        let suggestions: Vec<Suggestion> =
            (0..5).map(|i| make(&format!("item{i}"), None)).collect();
        let layout = compute_layout(
            &suggestions,
            0,
            22,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
        );
        // Layout always places below — start_row = cursor_row + 1
        assert_eq!(layout.start_row, 23);
        assert_eq!(layout.scroll_deficit, 0);
    }
```

- [ ] **Step 4: Implement scroll logic in `render_popup()`**

Replace the current `render_popup()` function (lines 67-122) with the scroll-aware version. Also add `prior_deficit: u16` parameter:

```rust
/// Render a popup into a byte buffer. Returns the layout used for positioning
/// (needed later for cleanup).
///
/// `prior_deficit` is the scroll deficit from a previous render in the same
/// popup session (e.g., during tab-cycling). It prevents re-scrolling by
/// accounting for viewport shifts that the parser doesn't know about.
#[allow(clippy::too_many_arguments)]
pub fn render_popup(
    buf: &mut Vec<u8>,
    suggestions: &[Suggestion],
    state: &OverlayState,
    cursor_row: u16,
    cursor_col: u16,
    screen_rows: u16,
    screen_cols: u16,
    max_visible: usize,
    min_width: u16,
    max_width: u16,
    theme: &PopupTheme,
    prior_deficit: u16,
) -> PopupLayout {
    if suggestions.is_empty() {
        return PopupLayout {
            start_row: 0,
            start_col: 0,
            width: 0,
            height: 0,
            scroll_deficit: 0,
        };
    }

    // Cap popup height to screen_rows - 1 (leave room for prompt row)
    let effective_max = if screen_rows > 1 {
        max_visible.min((screen_rows - 1) as usize)
    } else {
        return PopupLayout {
            start_row: 0,
            start_col: 0,
            width: 0,
            height: 0,
            scroll_deficit: 0,
        };
    };

    // Adjust cursor for prior scroll (parser doesn't know about our scrolling)
    let adj_cursor_row = cursor_row.saturating_sub(prior_deficit);

    // Calculate how much more scrolling is needed
    let space_below = screen_rows.saturating_sub(adj_cursor_row + 1);
    let visible_count = suggestions.len().min(effective_max) as u16;
    let new_deficit = visible_count.saturating_sub(space_below);
    let total_deficit = prior_deficit + new_deficit;
    let final_cursor_row = cursor_row.saturating_sub(total_deficit);

    ansi::begin_sync(buf);

    // Scroll viewport if we need more room
    if new_deficit > 0 {
        // Move to last viewport row so newlines cause actual scrolls
        ansi::move_to(buf, screen_rows - 1, 0);
        for _ in 0..new_deficit {
            buf.push(b'\n');
        }
        // Reposition cursor to the adjusted prompt location
        ansi::move_to(buf, final_cursor_row, cursor_col);
    }

    // Save cursor AFTER scroll repositioning
    ansi::save_cursor(buf);

    let layout = layout::compute_layout(
        suggestions,
        state.scroll_offset,
        final_cursor_row,
        cursor_col,
        screen_rows,
        screen_cols,
        effective_max,
        min_width,
        max_width,
    );

    if layout.height == 0 {
        ansi::restore_cursor(buf);
        ansi::end_sync(buf);
        return PopupLayout {
            scroll_deficit: total_deficit,
            ..layout
        };
    }

    let end = (state.scroll_offset + layout.height as usize).min(suggestions.len());
    let visible = &suggestions[state.scroll_offset..end];

    for (i, suggestion) in visible.iter().enumerate() {
        let row = layout.start_row + i as u16;
        let is_selected = state.selected == Some(state.scroll_offset + i);

        ansi::move_to(buf, row, layout.start_col);

        if is_selected {
            buf.extend_from_slice(&theme.selected_on);
        }

        format_item(buf, suggestion, layout.width, is_selected, theme);

        ansi::reset(buf);
    }

    ansi::restore_cursor(buf);
    ansi::end_sync(buf);

    PopupLayout {
        scroll_deficit: total_deficit,
        ..layout
    }
}
```

- [ ] **Step 5: Update ALL existing render.rs tests**

Add `, 0` (prior_deficit) to every existing `render_popup()` call. There are 6 existing test functions that call it:
- `test_render_produces_sync_wrapper`
- `test_render_saves_restores_cursor`
- `test_render_positions_at_layout`
- `test_selected_item_has_reverse_video`
- `test_render_with_scroll_offset`
- `test_render_empty_suggestions`

Also update `PopupLayout` literals in `test_clear_writes_spaces` (line 441-447) and `test_clear_correct_dimensions` (line 460-466): replace `renders_above: false` with `scroll_deficit: 0`.

- [ ] **Step 6: Add new scroll tests**

Add these tests to the `mod tests` block in render.rs:

```rust
    #[test]
    fn test_render_scroll_when_deficit_needed() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions(); // 3 items
        let state = OverlayState::new();
        // cursor at row 22 on 24-row screen: space_below = 1, need 3, deficit = 2
        let layout = render_popup(
            &mut buf,
            &suggestions,
            &state,
            22, 0, 24, 80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &PopupTheme::default(),
            0,
        );
        let output = String::from_utf8_lossy(&buf);
        // Should CUP to last row before newlines
        assert!(output.contains("\x1b[24;1H"), "should CUP to last row: {output}");
        // Should contain newlines
        assert!(output.contains("\n\n"), "should emit deficit newlines: {output}");
        assert_eq!(layout.scroll_deficit, 2);
        // adj_row = 22 - 2 = 20, start_row = 21
        assert_eq!(layout.start_row, 21);
    }

    #[test]
    fn test_render_no_scroll_when_room_below() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions(); // 3 items
        let state = OverlayState::new();
        let layout = render_popup(
            &mut buf,
            &suggestions,
            &state,
            5, 0, 24, 80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &PopupTheme::default(),
            0,
        );
        let output = String::from_utf8_lossy(&buf);
        assert!(!output.contains("\x1b[24;1H"), "should not scroll when room below");
        assert_eq!(layout.scroll_deficit, 0);
        assert_eq!(layout.start_row, 6);
    }

    #[test]
    fn test_render_prior_deficit_prevents_rescroll() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions(); // 3 items
        let state = OverlayState::new();
        // prior_deficit=2: adjusted cursor = 22-2 = 20, space_below = 3, no new deficit
        let layout = render_popup(
            &mut buf,
            &suggestions,
            &state,
            22, 0, 24, 80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &PopupTheme::default(),
            2,
        );
        let output = String::from_utf8_lossy(&buf);
        assert!(!output.contains("\x1b[24;1H"), "should not re-scroll: {output}");
        assert_eq!(layout.scroll_deficit, 2); // carries forward
        assert_eq!(layout.start_row, 21);
    }

    #[test]
    fn test_render_incremental_deficit() {
        // First render: 3 items, cursor at row 22, screen 24 -> deficit = 2
        let mut buf1 = Vec::new();
        let suggestions_3 = make_suggestions(); // 3 items
        let state = OverlayState::new();
        let layout1 = render_popup(
            &mut buf1,
            &suggestions_3,
            &state,
            22, 0, 24, 80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &PopupTheme::default(),
            0,
        );
        assert_eq!(layout1.scroll_deficit, 2);

        // Second render: 8 items, same cursor, prior_deficit=2
        // adj_cursor = 22-2 = 20, space_below = 3, need 8, new_deficit = 5
        let mut buf2 = Vec::new();
        let suggestions_8: Vec<Suggestion> = (0..8)
            .map(|i| make(&format!("item{i}"), None, SuggestionKind::Command))
            .collect();
        let layout2 = render_popup(
            &mut buf2,
            &suggestions_8,
            &state,
            22, 0, 24, 80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &PopupTheme::default(),
            2, // prior_deficit from first render
        );
        let output2 = String::from_utf8_lossy(&buf2);
        // Should scroll 5 more (total = 2 + 5 = 7)
        assert!(output2.contains("\x1b[24;1H"), "should scroll for incremental deficit");
        assert_eq!(layout2.scroll_deficit, 7);
        // final_cursor = 22 - 7 = 15, start_row = 16
        assert_eq!(layout2.start_row, 16);
    }

    #[test]
    fn test_render_decsc_after_scroll_not_before() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions();
        let state = OverlayState::new();
        render_popup(
            &mut buf,
            &suggestions,
            &state,
            22, 0, 24, 80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &PopupTheme::default(),
            0,
        );
        let output = String::from_utf8_lossy(&buf);
        let cup_to_adjusted = "\x1b[21;1H"; // adj_row=20, ANSI row=21
        let decsc = "\x1b7";
        let cup_pos = output.find(cup_to_adjusted)
            .expect("should contain CUP to adjusted position");
        let decsc_pos = output.find(decsc)
            .expect("should contain DECSC");
        assert!(decsc_pos > cup_pos, "DECSC must come AFTER CUP to adjusted position");
    }

    #[test]
    fn test_render_small_terminal_caps_height() {
        let mut buf = Vec::new();
        let suggestions: Vec<Suggestion> = (0..15)
            .map(|i| make(&format!("item{i}"), None, SuggestionKind::Command))
            .collect();
        let state = OverlayState::new();
        let layout = render_popup(
            &mut buf,
            &suggestions,
            &state,
            4, 0,
            6, // only 6 rows
            80,
            15, // max_visible bigger than screen
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &PopupTheme::default(),
            0,
        );
        // capped to screen_rows - 1 = 5
        assert!(layout.height <= 5, "height {} should be <= 5", layout.height);
        assert!(layout.start_row >= 1);
    }

    #[test]
    fn test_render_adj_row_never_underflows() {
        let mut buf = Vec::new();
        // cursor at row 2, 6-row terminal, 15 suggestions, max_visible=15
        // effective_max = min(15, 5) = 5
        // adj_cursor = 2, space_below = 6-2-1 = 3, visible=5, deficit = 2
        // total_deficit = 2, final_cursor = 2 - 2 = 0 (not underflowed)
        let suggestions: Vec<Suggestion> = (0..15)
            .map(|i| make(&format!("item{i}"), None, SuggestionKind::Command))
            .collect();
        let state = OverlayState::new();
        let layout = render_popup(
            &mut buf,
            &suggestions,
            &state,
            2, 0, 6, 80,
            15,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &PopupTheme::default(),
            0,
        );
        // final_cursor = 0, start_row = 1
        assert_eq!(layout.start_row, 1);
        assert_eq!(layout.scroll_deficit, 2);
    }
```

- [ ] **Step 7: Run all gc-overlay tests**

Run: `cargo test -p gc-overlay`
Expected: All pass

- [ ] **Step 8: Commit all gc-overlay changes together**

```bash
git add crates/gc-overlay/src/types.rs crates/gc-overlay/src/layout.rs crates/gc-overlay/src/render.rs
git commit -m "feat: scroll-to-make-room — never render above, scroll viewport instead

Remove above-cursor popup rendering path. When popup doesn't fit below,
scroll the viewport by emitting newlines from the last row, then render
below. Track scroll_deficit in PopupLayout to prevent drift during
tab-cycling. Cap popup height to screen_rows-1 for small terminals."
```

---

## Chunk 2: Handler Integration and Final Verification

### Task 2: Update handler to pass prior_deficit and dismiss on resize

**Files:**
- Modify: `crates/gc-pty/src/handler.rs:472-503` (render_at)
- Modify: `crates/gc-pty/src/handler.rs:546-550` (handle_resize)
- Modify: `crates/gc-pty/src/handler.rs:580-1065` (tests)

- [ ] **Step 1: Update `render_at()` — pass `prior_deficit` from `last_layout`**

Replace the `render_at` method (lines 472-503):

```rust
    fn render_at(
        &mut self,
        stdout: &mut dyn Write,
        cursor_row: u16,
        cursor_col: u16,
        screen_rows: u16,
        screen_cols: u16,
    ) {
        let prior_deficit = self
            .last_layout
            .as_ref()
            .map(|l| l.scroll_deficit)
            .unwrap_or(0);

        if let Some(ref layout) = self.last_layout {
            let mut clear_buf = Vec::new();
            clear_popup(&mut clear_buf, layout);
            let _ = stdout.write_all(&clear_buf);
        }

        let mut render_buf = Vec::new();
        let layout = render_popup(
            &mut render_buf,
            &self.suggestions,
            &self.overlay,
            cursor_row,
            cursor_col,
            screen_rows,
            screen_cols,
            self.max_visible,
            self.min_width,
            self.max_width,
            &self.theme,
            prior_deficit,
        );
        let _ = stdout.write_all(&render_buf);
        let _ = stdout.flush();
        self.last_layout = Some(layout);
    }
```

- [ ] **Step 2: Update `handle_resize()` — dismiss instead of re-render**

Replace lines 546-550:

```rust
    pub fn handle_resize(&mut self, _parser: &Arc<Mutex<TerminalParser>>, stdout: &mut dyn Write) {
        if self.visible {
            self.dismiss(stdout);
        }
    }
```

- [ ] **Step 3: Update all `PopupLayout` literals in tests — replace `renders_above` with `scroll_deficit`**

In handler.rs tests, replace `renders_above: false` with `scroll_deficit: 0` in all 5 occurrences:
- `test_dismiss_clears_state` (line ~674)
- `test_tab_accept_directory_predicts_buffer` (line ~785)
- `test_tab_accept_file_dismisses` (line ~842)
- `test_enter_no_selection_forwards_enter` (line ~898)
- `test_tab_no_selection_forwards_tab` (line ~942)

- [ ] **Step 4: Run all workspace tests**

Run: `cargo test`
Expected: All pass

**Note on handler-level `prior_deficit` wiring test:** `render_at()` is private and requires a full `SuggestionEngine` with controlled results to test directly. The `prior_deficit` passthrough is 3 lines of trivial code. Coverage comes from: (1) `render_popup()` unit tests that exhaustively verify deficit math with `prior_deficit` parameter, (2) the manual smoke test in Task 4 which validates the full tab-cycling flow end-to-end. If the wiring is wrong, the smoke test will immediately show progressive viewport drift.

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: Clean

- [ ] **Step 6: Run fmt**

Run: `cargo fmt --check`
Expected: Clean

- [ ] **Step 7: Commit**

```bash
git add crates/gc-pty/src/handler.rs
git commit -m "feat: integrate scroll-to-make-room in handler, dismiss on resize

Pass prior_deficit from last_layout to render_popup to prevent
re-scrolling during tab-cycling. Change handle_resize to dismiss
popup instead of re-rendering with stale deficit."
```

---

### Task 3: Update lib.rs doc comment

**Files:**
- Modify: `crates/gc-overlay/src/lib.rs:1-4`

- [ ] **Step 1: Update module doc — remove mention of above/below positioning**

```rust
//! ANSI-based popup rendering for terminal autocomplete.
//!
//! Renders suggestion popups using cursor save/restore, synchronized output
//! (DECSET 2026), and viewport scrolling to ensure popups always render below
//! the cursor without destroying scrollback content.
```

- [ ] **Step 2: Commit**

```bash
git add crates/gc-overlay/src/lib.rs
git commit -m "docs: update gc-overlay module doc for scroll-to-make-room"
```

---

### Task 4: Final verification

- [ ] **Step 1: Run full test suite**

Run: `cargo test`
Expected: All tests pass (332+ tests)

- [ ] **Step 2: Run clippy with deny warnings**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: Clean

- [ ] **Step 3: Run fmt**

Run: `cargo fmt --check`
Expected: Clean

- [ ] **Step 4: Build release binary**

Run: `cargo build --release`
Expected: Compiles successfully

- [ ] **Step 5: Manual smoke test**

```bash
cp target/release/ghost-complete ~/.cargo/bin/ && codesign -f -s - ~/.cargo/bin/ghost-complete
```

Open Ghostty, run `ghost-complete`, navigate to a directory with many items. Position cursor near bottom of screen. Trigger completion. Verify:
- Popup appears below cursor (never above)
- Terminal scrolls up to make room if needed
- Tab-cycling doesn't cause progressive viewport drift
- Dismissing popup leaves no blank holes in scrollback
- Scrolling up shows all original content intact
