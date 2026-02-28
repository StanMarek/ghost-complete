# Phase 5 Design: gc-overlay — Popup Rendering Library

## Summary

Pure rendering library that takes suggestions + terminal state and produces ANSI byte sequences into a `Vec<u8>`. Does not own stdout, manage visibility state, or intercept keystrokes.

## Design Decisions

- **Borderless popup** with reverse-video selection, dim descriptions
- **Cursor-column positioning** — popup appears below/above where user is typing
- **Synchronized output** (DECSET 2026) for flicker-free rendering on Ghostty
- **Cursor save/restore** (DECSC/DECRC) so cursor returns to original position
- **Write to Vec<u8>** — caller owns stdout, overlay just builds escape sequences
- **Cleanup via space overwrite** — no ED/EL to avoid scrollback corruption

## Public API

```rust
pub struct OverlayState {
    pub selected: usize,
    pub scroll_offset: usize,
    pub visible_count: usize,
}

pub struct PopupLayout {
    pub start_row: u16,
    pub start_col: u16,
    pub width: u16,
    pub height: u16,
    pub renders_above: bool,
}

pub fn render_popup(
    buf: &mut Vec<u8>,
    suggestions: &[Suggestion],
    state: &OverlayState,
    cursor_row: u16,
    cursor_col: u16,
    screen_rows: u16,
    screen_cols: u16,
) -> PopupLayout;

pub fn clear_popup(buf: &mut Vec<u8>, layout: &PopupLayout);
```

## Rendering Strategy

1. Begin synchronized output (`\x1b[?2026h`)
2. Save cursor (`\x1b7`)
3. For each visible suggestion:
   - Move cursor to popup row/col (`\x1b[{row};{col}H`)
   - If selected: reverse video (`\x1b[7m`)
   - Write kind gutter + text + description
   - Reset attributes (`\x1b[0m`)
4. Restore cursor (`\x1b8`)
5. End synchronized output (`\x1b[?2026l`)

## Positioning Logic

- Default: below cursor (`cursor_row + 1`)
- If `space_below < popup_height`: render above (`cursor_row - popup_height`)
- If `cursor_col + popup_width > screen_cols`: shift left
- Max visible items: 10

## Cleanup Strategy

- Move cursor to each popup row
- Write spaces for the full popup width
- Restore cursor

## File Layout

```
crates/gc-overlay/src/
  lib.rs      — public API re-exports
  types.rs    — OverlayState, PopupLayout
  render.rs   — render_popup(), item formatting
  layout.rs   — compute_layout() positioning
  cleanup.rs  — clear_popup()
  ansi.rs     — ANSI sequence helpers
```

## Out of Scope

- Popup visibility state machine (Phase 6)
- Keystroke interception (Phase 6)
- Trigger detection (Phase 6)
- Color theming/config (Phase 7)
