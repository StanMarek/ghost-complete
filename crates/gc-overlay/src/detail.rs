//! Adjacent description box rendering.
//!
//! Renders a multi-line wrapped description for the currently-selected
//! suggestion next to (or below) the main popup. Inspired by inshellisense's
//! 40+30 split; geometry adapts to terminal width and falls back through a
//! side → below → hide cascade.
//!
//! The detail box is rendered in the same sync window as the main popup —
//! callers should wrap both `clear_popup`/`render_popup` and the matching
//! `clear_detail_box`/`render_detail_box` calls inside their existing
//! `begin_sync`/`end_sync` boundary (or single PreRenderBuffer flush) so
//! atomicity holds.

use std::io::Write;

use gc_suggest::Suggestion;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::ansi;
use crate::layout::{DESC_GAP_COLS, GUTTER_COLS, TRAILING_PAD_COLS};
use crate::render::{sanitize_display_text, PopupTheme};
use crate::types::PopupLayout;
use crate::util::{display_text, TRUNCATION_ELLIPSIS};

/// Minimum useful detail-box content width. Narrower boxes are too cramped to
/// be worth the screen real estate; geometry falls through to the next tier
/// of the side/below/hide cascade.
pub const MIN_USEFUL_WIDTH: u16 = 30;

/// Returns `true` when the suggestion's description would be truncated in
/// its inline row given the supplied popup geometry — i.e. the description
/// can't fit alongside the suggestion text within the main popup's
/// per-row description budget.
///
/// Used by the proxy as a gate for the optional adjacent description box:
/// when the inline description fits cleanly, there is no useful information
/// for the side box to add, so we skip rendering it. Mirrors the budget
/// computation in `render::write_description` exactly — including the
/// scrollbar-presence check, which depends on `effective_max = min(
/// max_visible, screen_rows - 1 - border_pad)`, not on `max_visible`
/// alone — so the gate cannot disagree with the inline truncation logic.
///
/// Returns `false` when the suggestion has no description, the description
/// is empty, or the popup is zero-sized (no inline truncation can happen).
pub fn description_overflows_main_popup(
    suggestion: &Suggestion,
    popup_layout: &PopupLayout,
    total_suggestions: usize,
    max_visible: usize,
    borders: bool,
    screen_rows: u16,
) -> bool {
    let Some(desc) = suggestion.description.as_deref() else {
        return false;
    };
    if desc.is_empty() {
        return false;
    }
    if popup_layout.width == 0 {
        return false;
    }
    let border_pad: u16 = if borders { 2 } else { 0 };
    // Match `render::render_popup`'s `effective_max` formula exactly: the
    // popup viewport is capped by both the user's `max_visible` knob and
    // the screen height minus the prompt row and any border padding. On a
    // tight screen the screen-height term dominates and the scrollbar
    // appears at a smaller suggestion count than `max_visible` alone would
    // suggest.
    let min_screen = 1 + border_pad;
    let effective_max = if screen_rows > min_screen {
        max_visible.min(screen_rows.saturating_sub(min_screen) as usize)
    } else {
        // Tiny screen: render_popup suppresses the popup entirely (zero
        // height). We're past that branch (popup_layout.width > 0 here),
        // but treat as worst-case "no scrollbar" since there's nothing
        // sensible to compare against.
        max_visible
    };
    let total_width = popup_layout.width as usize;
    let content_width = total_width.saturating_sub(border_pad as usize);
    let needs_scrollbar = total_suggestions > effective_max.max(1);
    let item_width = if needs_scrollbar {
        content_width.saturating_sub(1)
    } else {
        content_width
    };
    let (dt, _) = display_text(suggestion);
    let text_cols = UnicodeWidthStr::width(dt);
    let gutter_text_len = GUTTER_COLS + text_cols;
    let max_desc_cols = item_width
        .saturating_sub(gutter_text_len)
        .saturating_sub(DESC_GAP_COLS)
        .saturating_sub(TRAILING_PAD_COLS);
    let desc_cols = UnicodeWidthStr::width(desc);
    desc_cols > max_desc_cols
}

/// Where the detail box landed relative to the main popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailPosition {
    /// Right of the main popup, anchored at `main.start_row`.
    SideRight,
    /// Left of the main popup, anchored at `main.start_row`.
    SideLeft,
    /// Below the main popup, anchored at `main.start_row + main.height`.
    Below,
}

#[derive(Debug, Clone)]
pub struct DetailLayout {
    pub start_row: u16,
    pub start_col: u16,
    pub width: u16,
    pub height: u16,
    pub position: DetailPosition,
}

/// Compute the detail-box layout as a side → below → None cascade.
///
/// Returns `None` when:
/// - `main` is zero-sized (popup suppressed)
/// - No tier of the cascade has room
/// - `lines == 0`
///
/// Border padding is included in the returned `width` and `height`. The
/// detail box uses borders independently from the main popup; both cases
/// use the same `borders` flag for visual consistency.
pub fn compute_detail_layout(
    main: &PopupLayout,
    screen_rows: u16,
    screen_cols: u16,
    max_width: u16,
    lines: u16,
    borders: bool,
) -> Option<DetailLayout> {
    if main.height == 0 || main.width == 0 || lines == 0 || max_width == 0 {
        return None;
    }
    let border_pad: u16 = if borders { 2 } else { 0 };
    let needed_height = lines + border_pad;
    if needed_height == 0 {
        return None;
    }

    // --- Tier 1: side-right ---
    let main_end_col = main.start_col.saturating_add(main.width);
    // 1-col gap between main and detail keeps the visual seam clean.
    let cols_to_right = screen_cols.saturating_sub(main_end_col).saturating_sub(1);
    if cols_to_right >= MIN_USEFUL_WIDTH {
        let width = cols_to_right.min(max_width);
        let height = needed_height.min(screen_rows.saturating_sub(main.start_row));
        if height > border_pad {
            return Some(DetailLayout {
                start_row: main.start_row,
                start_col: main_end_col + 1,
                width,
                height,
                position: DetailPosition::SideRight,
            });
        }
    }

    // --- Tier 2: side-left ---
    // Need MIN_USEFUL_WIDTH cols + 1 gap to the left of the main popup.
    if main.start_col > MIN_USEFUL_WIDTH {
        let available = main.start_col.saturating_sub(1);
        let width = available.min(max_width);
        let height = needed_height.min(screen_rows.saturating_sub(main.start_row));
        if height > border_pad {
            return Some(DetailLayout {
                start_row: main.start_row,
                start_col: main.start_col.saturating_sub(width).saturating_sub(1),
                width,
                height,
                position: DetailPosition::SideLeft,
            });
        }
    }

    // --- Tier 3: below ---
    // Anchor below the main popup, matching its left edge. We don't trigger
    // additional viewport scrolling here — if there isn't already room, hide.
    let main_bottom = main.start_row.saturating_add(main.height);
    let rows_below = screen_rows.saturating_sub(main_bottom);
    if rows_below > border_pad {
        let height = needed_height.min(rows_below);
        let avail_cols = screen_cols.saturating_sub(main.start_col);
        let width = main
            .width
            .max(MIN_USEFUL_WIDTH)
            .min(max_width)
            .min(avail_cols);
        if width >= MIN_USEFUL_WIDTH {
            return Some(DetailLayout {
                start_row: main_bottom,
                start_col: main.start_col,
                width,
                height,
                position: DetailPosition::Below,
            });
        }
    }

    None
}

/// Word-wrap `text` to fit within `max_width` display columns over at most
/// `max_lines` lines. Long descriptions are truncated with a single-column
/// ellipsis (`…`) on the final line.
///
/// Whitespace runs collapse to single spaces (the source corpus has no
/// embedded newlines per the description-data analysis on issue #116). A
/// word longer than `max_width` is hard-broken char-by-char.
pub fn wrap_description(text: &str, max_width: usize, max_lines: usize) -> Vec<String> {
    if max_width == 0 || max_lines == 0 || text.trim().is_empty() {
        return Vec::new();
    }

    let mut lines: Vec<String> = Vec::with_capacity(max_lines);
    let mut current = String::new();
    let mut current_cols: usize = 0;
    let mut overflow = false;

    'outer: for word in text.split_whitespace() {
        let word_cols: usize = word
            .chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum();
        if word_cols == 0 {
            continue;
        }
        let needs_space = !current.is_empty();
        let separator_cols = if needs_space { 1 } else { 0 };

        if word_cols <= max_width && current_cols + separator_cols + word_cols <= max_width {
            if needs_space {
                current.push(' ');
                current_cols += 1;
            }
            current.push_str(word);
            current_cols += word_cols;
            continue;
        }

        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current_cols = 0;
            if lines.len() >= max_lines {
                overflow = true;
                break 'outer;
            }
        }

        if word_cols <= max_width {
            current.push_str(word);
            current_cols = word_cols;
        } else {
            for ch in word.chars() {
                let w = UnicodeWidthChar::width(ch).unwrap_or(0);
                if current_cols + w > max_width {
                    if current.is_empty() {
                        // Single zero/over-wide char — drop and continue so
                        // we don't infinite-loop on pathological data.
                        break;
                    }
                    lines.push(std::mem::take(&mut current));
                    current_cols = 0;
                    if lines.len() >= max_lines {
                        overflow = true;
                        break 'outer;
                    }
                }
                current.push(ch);
                current_cols += w;
            }
        }
    }

    if !current.is_empty() {
        if lines.len() < max_lines {
            lines.push(current);
        } else {
            overflow = true;
        }
    }

    if overflow {
        if let Some(last) = lines.last_mut() {
            let mut cols = UnicodeWidthStr::width(last.as_str());
            while cols + 1 > max_width {
                let Some(c) = last.pop() else { break };
                cols = cols.saturating_sub(UnicodeWidthChar::width(c).unwrap_or(0));
            }
            last.push(TRUNCATION_ELLIPSIS);
        }
    }

    lines
}

/// Render the detail box into `buf`. Sanitizes `description` against ANSI
/// injection (mirrors `write_description` in render.rs).
///
/// The caller is responsible for sync framing — this function only emits
/// cursor moves, optional border glyphs, and the wrapped content rows.
pub fn render_detail_box(
    buf: &mut Vec<u8>,
    layout: &DetailLayout,
    description: &str,
    theme: &PopupTheme,
) {
    if layout.width == 0 || layout.height == 0 {
        return;
    }
    let border_pad: u16 = if theme.borders { 2 } else { 0 };
    if layout.height <= border_pad {
        return;
    }
    let content_width = layout.width.saturating_sub(border_pad) as usize;
    let content_height = layout.height.saturating_sub(border_pad) as usize;
    if content_width == 0 || content_height == 0 {
        return;
    }

    ansi::save_cursor(buf);

    let sanitized = sanitize_display_text(description);
    let lines = wrap_description(&sanitized, content_width, content_height);

    // Top border
    if theme.borders {
        ansi::move_to(buf, layout.start_row, layout.start_col);
        if !theme.border_on.is_empty() {
            buf.extend_from_slice(&theme.border_on);
        }
        buf.extend_from_slice("╭".as_bytes());
        for _ in 0..content_width {
            buf.extend_from_slice("─".as_bytes());
        }
        buf.extend_from_slice("╮".as_bytes());
        ansi::reset(buf);
    }

    let content_start_row = layout.start_row + u16::from(theme.borders);
    for row_idx in 0..content_height {
        let row = content_start_row + row_idx as u16;
        ansi::move_to(buf, row, layout.start_col);
        if theme.borders {
            if !theme.border_on.is_empty() {
                buf.extend_from_slice(&theme.border_on);
            }
            buf.extend_from_slice("│".as_bytes());
            ansi::reset(buf);
        }
        if !theme.description_on.is_empty() {
            buf.extend_from_slice(&theme.description_on);
        }
        let line_text = lines.get(row_idx).map(String::as_str).unwrap_or("");
        let _ = write!(buf, "{line_text}");
        let line_cols = UnicodeWidthStr::width(line_text);
        let pad = content_width.saturating_sub(line_cols);
        for _ in 0..pad {
            buf.push(b' ');
        }
        ansi::reset(buf);
        if theme.borders {
            if !theme.border_on.is_empty() {
                buf.extend_from_slice(&theme.border_on);
            }
            buf.extend_from_slice("│".as_bytes());
            ansi::reset(buf);
        }
    }

    // Bottom border
    if theme.borders {
        let bottom_row = layout.start_row + layout.height - 1;
        ansi::move_to(buf, bottom_row, layout.start_col);
        if !theme.border_on.is_empty() {
            buf.extend_from_slice(&theme.border_on);
        }
        buf.extend_from_slice("╰".as_bytes());
        for _ in 0..content_width {
            buf.extend_from_slice("─".as_bytes());
        }
        buf.extend_from_slice("╯".as_bytes());
        ansi::reset(buf);
    }

    ansi::restore_cursor(buf);
}

/// Surgical clear of the detail-box rectangle. Mirrors `clear_popup` in
/// render.rs — overwrites with spaces, never uses `\x1b[2J` so scrollback
/// stays intact.
pub fn clear_detail_box(buf: &mut Vec<u8>, layout: &DetailLayout) {
    if layout.width == 0 || layout.height == 0 {
        return;
    }
    ansi::save_cursor(buf);
    for row in 0..layout.height {
        ansi::move_to(buf, layout.start_row + row, layout.start_col);
        for _ in 0..layout.width {
            buf.push(b' ');
        }
    }
    ansi::restore_cursor(buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(start_row: u16, start_col: u16, width: u16, height: u16) -> PopupLayout {
        PopupLayout {
            start_row,
            start_col,
            width,
            height,
            scroll_deficit: 0,
        }
    }

    fn make_suggestion(text: &str, desc: Option<&str>) -> Suggestion {
        Suggestion {
            text: text.to_string(),
            description: desc.map(String::from),
            ..Default::default()
        }
    }

    // --- description_overflows_main_popup ---

    #[test]
    fn overflow_check_short_description_fits() {
        // 60-col popup, "git" (3 chars) + 2-col gap + "current branch" (14) + 1-col pad + 4-col gutter = 24 cols.
        // Plenty of room — no overflow.
        let s = make_suggestion("git", Some("current branch"));
        let pl = layout(0, 0, 60, 10);
        assert!(!description_overflows_main_popup(&s, &pl, 1, 10, false, 24));
    }

    #[test]
    fn overflow_check_long_description_truncates() {
        // Tight 30-col popup with a long description — must overflow.
        let s = make_suggestion(
            "checkout",
            Some("Switch branches or restore working tree files with this very long description"),
        );
        let pl = layout(0, 0, 30, 10);
        assert!(description_overflows_main_popup(&s, &pl, 1, 10, false, 24));
    }

    #[test]
    fn overflow_check_no_description_returns_false() {
        let s = make_suggestion("git", None);
        let pl = layout(0, 0, 60, 10);
        assert!(!description_overflows_main_popup(&s, &pl, 1, 10, false, 24));
    }

    #[test]
    fn overflow_check_empty_description_returns_false() {
        let s = make_suggestion("git", Some(""));
        let pl = layout(0, 0, 60, 10);
        assert!(!description_overflows_main_popup(&s, &pl, 1, 10, false, 24));
    }

    #[test]
    fn overflow_check_zero_width_popup_returns_false() {
        let s = make_suggestion("git", Some("anything"));
        let pl = layout(0, 0, 0, 0);
        assert!(!description_overflows_main_popup(&s, &pl, 1, 10, false, 24));
    }

    #[test]
    fn overflow_check_scrollbar_eats_one_column() {
        // 30-col popup, 8-char text, 14-char description: gutter(4) + text(8) +
        // gap(2) + desc(14) + pad(1) = 29. Fits without scrollbar (item_width=30).
        // With scrollbar, item_width=29, budget for desc = 29-4-8-2-1 = 14. Still fits.
        // Bump to 15-char description: 15 > 14 → overflows only with scrollbar.
        let s = make_suggestion("checkout", Some("123456789012345"));
        let pl = layout(0, 0, 30, 10);
        // Without scrollbar (1 suggestion): budget = 30-4-8-2-1 = 15, desc=15 → no overflow.
        assert!(!description_overflows_main_popup(&s, &pl, 1, 10, false, 24));
        // With scrollbar (more suggestions than max_visible): budget = 14, desc=15 → overflow.
        assert!(description_overflows_main_popup(&s, &pl, 20, 10, false, 24));
    }

    #[test]
    fn overflow_check_tight_screen_triggers_scrollbar_below_max_visible() {
        // Regression: render::render_popup uses effective_max = min(max_visible,
        // screen_rows - 1 - border_pad). On a tight screen the screen-height
        // term dominates and the scrollbar appears even when total_suggestions
        // <= max_visible. The overflow check must mirror that formula or it
        // silently suppresses detail boxes the user actually needs.
        //
        // Setup: 30-col popup, text="checkout"(8), desc=15 chars.
        //   - Without scrollbar: budget = 30-4-8-2-1 = 15, desc=15 → fits.
        //   - With scrollbar: budget = 14, desc=15 → overflows.
        // total=10, max_visible=20, screen_rows=8, borders=false:
        //   - effective_max = min(20, 8-1) = 7 → 10 > 7 → scrollbar present.
        //   - Naive heuristic (max_visible alone): 10 > 20 = false → wrong!
        let s = make_suggestion("checkout", Some("123456789012345"));
        let pl = layout(0, 0, 30, 10);
        assert!(description_overflows_main_popup(&s, &pl, 10, 20, false, 8));
        // Sanity check: same suggestion count + max_visible on a tall screen
        // does NOT trigger the scrollbar — so the gate correctly says "fits".
        assert!(!description_overflows_main_popup(
            &s, &pl, 10, 20, false, 100
        ));
    }

    #[test]
    fn overflow_check_borders_reduce_budget() {
        // 30-col popup, text="cmd" (3), description="0123456789012345678901234"
        // Without borders: budget = 30-4-3-2-1 = 20. desc=25 → overflow.
        // Already overflows without borders, so use a tighter case:
        // text="cmd" (3), description has 22 chars. Budget no borders = 20, desc=22 → overflow.
        // Border-pad adds 2 (1 each side) so budget = 28-4-3-2-1 = 18 → overflow even more. Both true.
        // Try description that ONLY overflows with borders:
        // text="cmd"(3), description=20 chars. No borders: budget=20, desc=20 → no overflow.
        // Borders: budget = (30-2)-4-3-2-1 = 18, desc=20 → overflow.
        let s = make_suggestion("cmd", Some("12345678901234567890"));
        let pl = layout(0, 0, 30, 10);
        assert!(!description_overflows_main_popup(&s, &pl, 1, 10, false, 24));
        assert!(description_overflows_main_popup(&s, &pl, 1, 10, true, 24));
    }

    #[test]
    fn overflow_check_cjk_description_width() {
        // 4 CJK chars = 8 cols. Tight popup forces overflow.
        let s = make_suggestion("cmd", Some("\u{65E5}\u{672C}\u{8A9E}\u{6F22}"));
        let pl = layout(0, 0, 18, 10);
        // budget = 18-4-3-2-1 = 8, desc cols = 8 → exact fit, no overflow.
        assert!(!description_overflows_main_popup(&s, &pl, 1, 10, false, 24));
        // Trim popup by 1 col → budget = 7, desc = 8 → overflow.
        let pl2 = layout(0, 0, 17, 10);
        assert!(description_overflows_main_popup(&s, &pl2, 1, 10, false, 24));
    }

    // --- compute_detail_layout cascade ---

    #[test]
    fn detail_lands_side_right_on_wide_terminal() {
        // 200-col screen, 60-col main popup at col 0 → 139 cols to the right.
        let main = layout(5, 0, 60, 10);
        let det = compute_detail_layout(&main, 24, 200, 60, 5, false).unwrap();
        assert_eq!(det.position, DetailPosition::SideRight);
        assert_eq!(det.start_col, 61); // main_end_col + 1
        assert_eq!(det.width, 60);
        assert_eq!(det.height, 5);
    }

    #[test]
    fn detail_lands_side_left_when_no_room_right_but_room_left() {
        // Main popup pinned to the right edge: very little room to the right,
        // plenty to the left.
        let main = layout(5, 60, 20, 10);
        // screen_cols = 80, main_end_col = 80, cols_to_right = -1 (saturated to 0)
        let det = compute_detail_layout(&main, 24, 80, 60, 5, false).unwrap();
        assert_eq!(det.position, DetailPosition::SideLeft);
        // 60 cols available to the left, capped at max_width (60). Box ends at
        // main.start_col - 1 = 59, box of width 59 starts at col 0.
        assert!(det.start_col + det.width < main.start_col);
    }

    #[test]
    fn detail_falls_back_to_below_on_narrow_terminal() {
        // 60-col screen, 60-col main: 0 cols on either side → must stack below.
        let main = layout(5, 0, 60, 10);
        let det = compute_detail_layout(&main, 24, 60, 60, 5, false).unwrap();
        assert_eq!(det.position, DetailPosition::Below);
        assert_eq!(det.start_row, 15); // main.start_row + main.height
        assert_eq!(det.start_col, 0);
    }

    #[test]
    fn detail_hides_when_no_room_anywhere() {
        // Tiny screen: main popup eats most of it.
        let main = layout(0, 0, 60, 24);
        let det = compute_detail_layout(&main, 24, 60, 60, 5, false);
        assert!(det.is_none());
    }

    #[test]
    fn detail_hides_when_main_is_zero_sized() {
        let main = layout(0, 0, 0, 0);
        assert!(compute_detail_layout(&main, 24, 80, 60, 5, false).is_none());
    }

    #[test]
    fn detail_clamps_width_to_max() {
        // Plenty of room to the right; max_width=40 should cap.
        let main = layout(0, 0, 60, 10);
        let det = compute_detail_layout(&main, 24, 200, 40, 5, false).unwrap();
        assert_eq!(det.width, 40);
    }

    #[test]
    fn detail_caps_height_at_remaining_screen_rows() {
        // Main starts at row 20 on a 24-row screen → only 4 rows available.
        let main = layout(20, 0, 30, 4);
        let det = compute_detail_layout(&main, 24, 200, 60, 10, false).unwrap();
        assert_eq!(det.position, DetailPosition::SideRight);
        assert!(det.height <= 4);
    }

    #[test]
    fn detail_zero_lines_returns_none() {
        let main = layout(0, 0, 60, 10);
        assert!(compute_detail_layout(&main, 24, 200, 60, 0, false).is_none());
    }

    #[test]
    fn detail_below_requires_min_useful_width() {
        // Main popup of width 20, narrow terminal → "below" position with main
        // width 20 < MIN_USEFUL_WIDTH 30 should fall through to None.
        let main = layout(0, 0, 20, 5);
        // screen_cols = 50: cols_to_right = 50-20-1=29 < 30 (no side-right);
        // start_col=0, no side-left; below width = max(20, 30)=30 fits.
        let det = compute_detail_layout(&main, 24, 50, 60, 5, false);
        assert!(det.is_some());
        assert_eq!(det.unwrap().position, DetailPosition::Below);
    }

    // --- wrap_description ---

    #[test]
    fn wrap_short_description_one_line() {
        let lines = wrap_description("Hello world", 80, 5);
        assert_eq!(lines, vec!["Hello world"]);
    }

    #[test]
    fn wrap_breaks_on_word_boundary() {
        let lines = wrap_description("Hello world from Rust", 12, 5);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "Hello world");
        assert_eq!(lines[1], "from Rust");
    }

    #[test]
    fn wrap_truncates_with_ellipsis_at_max_lines() {
        let lines = wrap_description(
            "one two three four five six seven eight nine ten eleven twelve",
            10,
            2,
        );
        assert_eq!(lines.len(), 2);
        // Last line ends with ellipsis.
        assert!(lines[1].ends_with(TRUNCATION_ELLIPSIS));
    }

    #[test]
    fn wrap_empty_input_yields_no_lines() {
        assert!(wrap_description("", 80, 5).is_empty());
        assert!(wrap_description("   ", 80, 5).is_empty());
    }

    #[test]
    fn wrap_zero_width_or_lines_yields_no_lines() {
        assert!(wrap_description("hello", 0, 5).is_empty());
        assert!(wrap_description("hello", 80, 0).is_empty());
    }

    #[test]
    fn wrap_hard_breaks_long_word() {
        // Word of 30 chars in a 10-col budget — must break char-by-char.
        let word: String = "a".repeat(30);
        let lines = wrap_description(&word, 10, 5);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].chars().count(), 10);
    }

    #[test]
    fn wrap_handles_cjk_width() {
        // 4 CJK chars = 8 cols; budget = 6 → two lines of 3 chars each
        // (because 3 chars × 2 cols = 6 fits exactly).
        let lines = wrap_description("\u{65E5}\u{672C}\u{8A9E}\u{6F22}", 6, 5);
        assert_eq!(lines.len(), 2);
        for line in &lines {
            assert!(UnicodeWidthStr::width(line.as_str()) <= 6);
        }
    }

    #[test]
    fn wrap_collapses_multiple_whitespace() {
        let lines = wrap_description("a    b\tc\n\nd", 80, 5);
        assert_eq!(lines, vec!["a b c d"]);
    }

    #[test]
    fn wrap_truncation_does_not_exceed_max_width() {
        let long: String = "lorem ipsum ".repeat(20);
        let lines = wrap_description(&long, 12, 3);
        assert_eq!(lines.len(), 3);
        for line in &lines {
            assert!(
                UnicodeWidthStr::width(line.as_str()) <= 12,
                "line {line:?} exceeds 12 cols"
            );
        }
    }

    // --- render/clear smoke ---

    #[test]
    fn render_emits_no_bytes_for_zero_size() {
        let layout = DetailLayout {
            start_row: 0,
            start_col: 0,
            width: 0,
            height: 0,
            position: DetailPosition::SideRight,
        };
        let mut buf = Vec::new();
        render_detail_box(&mut buf, &layout, "anything", &PopupTheme::default());
        assert!(buf.is_empty());
    }

    #[test]
    fn clear_emits_no_bytes_for_zero_size() {
        let layout = DetailLayout {
            start_row: 0,
            start_col: 0,
            width: 0,
            height: 0,
            position: DetailPosition::SideRight,
        };
        let mut buf = Vec::new();
        clear_detail_box(&mut buf, &layout);
        assert!(buf.is_empty());
    }

    #[test]
    fn clear_writes_spaces_over_full_rect() {
        let layout = DetailLayout {
            start_row: 5,
            start_col: 10,
            width: 4,
            height: 2,
            position: DetailPosition::SideRight,
        };
        let mut buf = Vec::new();
        clear_detail_box(&mut buf, &layout);
        // Should contain 8 spaces total (4 width × 2 height) plus cursor moves.
        let space_count = buf.iter().filter(|&&b| b == b' ').count();
        assert_eq!(space_count, 8);
        // Save/restore cursor wrappers (DECSC/DECRC).
        assert!(buf.starts_with(b"\x1b7"));
        assert!(buf.ends_with(b"\x1b8"));
    }

    #[test]
    fn render_with_borders_includes_box_drawing_chars() {
        let layout = DetailLayout {
            start_row: 0,
            start_col: 0,
            width: 10,
            height: 5,
            position: DetailPosition::SideRight,
        };
        let mut buf = Vec::new();
        let theme = PopupTheme {
            borders: true,
            ..PopupTheme::default()
        };
        render_detail_box(&mut buf, &layout, "hello", &theme);
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains('╭'), "missing top-left corner");
        assert!(s.contains('╯'), "missing bottom-right corner");
    }
}
