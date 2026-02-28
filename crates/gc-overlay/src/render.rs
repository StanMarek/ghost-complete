use std::io::Write;

use gc_suggest::{Suggestion, SuggestionKind};

use crate::ansi;
use crate::layout;
use crate::types::{OverlayState, PopupLayout};

/// Render a popup into a byte buffer. Returns the layout used for positioning
/// (needed later for cleanup).
pub fn render_popup(
    buf: &mut Vec<u8>,
    suggestions: &[Suggestion],
    state: &OverlayState,
    cursor_row: u16,
    cursor_col: u16,
    screen_rows: u16,
    screen_cols: u16,
) -> PopupLayout {
    let layout = layout::compute_layout(
        suggestions,
        state.scroll_offset,
        cursor_row,
        cursor_col,
        screen_rows,
        screen_cols,
    );

    if layout.height == 0 {
        return layout;
    }

    ansi::begin_sync(buf);
    ansi::save_cursor(buf);

    let end = (state.scroll_offset + layout.height as usize).min(suggestions.len());
    let visible = &suggestions[state.scroll_offset..end];

    for (i, suggestion) in visible.iter().enumerate() {
        let row = layout.start_row + i as u16;
        let is_selected = state.scroll_offset + i == state.selected;

        ansi::move_to(buf, row, layout.start_col);

        if is_selected {
            ansi::reverse_video(buf);
        }

        format_item(buf, suggestion, layout.width, is_selected);

        ansi::reset(buf);
    }

    ansi::restore_cursor(buf);
    ansi::end_sync(buf);

    layout
}

/// Clear the popup area by overwriting with spaces.
pub fn clear_popup(buf: &mut Vec<u8>, layout: &PopupLayout) {
    if layout.height == 0 {
        return;
    }

    ansi::begin_sync(buf);
    ansi::save_cursor(buf);

    for row_offset in 0..layout.height {
        let row = layout.start_row + row_offset;
        ansi::move_to(buf, row, layout.start_col);
        for _ in 0..layout.width {
            let _ = buf.write_all(b" ");
        }
    }

    ansi::restore_cursor(buf);
    ansi::end_sync(buf);
}

fn format_item(buf: &mut Vec<u8>, s: &Suggestion, width: u16, is_selected: bool) {
    let kind_char = match s.kind {
        SuggestionKind::Command => 'C',
        SuggestionKind::Subcommand => 'S',
        SuggestionKind::Flag => 'F',
        SuggestionKind::FilePath => 'f',
        SuggestionKind::Directory => 'd',
        SuggestionKind::GitBranch => 'B',
        SuggestionKind::GitTag => 'T',
        SuggestionKind::GitRemote => 'R',
        SuggestionKind::History => 'H',
    };

    // Gutter: " K "
    let _ = write!(buf, " {kind_char} ");

    // Text
    let text = &s.text;
    let _ = write!(buf, "{text}");

    let gutter_text_len = 3 + text.len();
    let total_width = width as usize;

    // Description (if room)
    let desc = s.description.as_deref().unwrap_or("");
    // Need at least 2 chars gap + 2 chars desc to bother showing it
    let max_desc_len = total_width.saturating_sub(gutter_text_len + 2 + 1);

    if !desc.is_empty() && max_desc_len > 2 {
        let _ = buf.write_all(b"  ");
        if !is_selected {
            ansi::dim(buf);
        }
        let truncated: String = desc.chars().take(max_desc_len).collect();
        let _ = write!(buf, "{truncated}");
        if !is_selected {
            ansi::reset(buf);
        }
        // Pad remaining
        let used = gutter_text_len + 2 + truncated.len();
        let pad = total_width.saturating_sub(used);
        for _ in 0..pad {
            let _ = buf.write_all(b" ");
        }
    } else {
        // No description — just pad
        let pad = total_width.saturating_sub(gutter_text_len);
        for _ in 0..pad {
            let _ = buf.write_all(b" ");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gc_suggest::SuggestionSource;

    fn make(text: &str, desc: Option<&str>, kind: SuggestionKind) -> Suggestion {
        Suggestion {
            text: text.to_string(),
            description: desc.map(String::from),
            kind,
            source: SuggestionSource::Spec,
            score: 0,
        }
    }

    fn make_suggestions() -> Vec<Suggestion> {
        vec![
            make(
                "checkout",
                Some("Switch branches"),
                SuggestionKind::Subcommand,
            ),
            make("commit", Some("Record changes"), SuggestionKind::Subcommand),
            make("push", Some("Update remote"), SuggestionKind::Subcommand),
        ]
    }

    #[test]
    fn test_render_produces_sync_wrapper() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions();
        let state = OverlayState::new();
        render_popup(&mut buf, &suggestions, &state, 5, 0, 24, 80);
        let output = String::from_utf8_lossy(&buf);
        assert!(
            output.starts_with("\x1b[?2026h"),
            "should start with begin_sync"
        );
        assert!(output.ends_with("\x1b[?2026l"), "should end with end_sync");
    }

    #[test]
    fn test_render_saves_restores_cursor() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions();
        let state = OverlayState::new();
        render_popup(&mut buf, &suggestions, &state, 5, 0, 24, 80);
        let output = String::from_utf8_lossy(&buf);
        assert!(output.contains("\x1b7"), "should contain save cursor");
        assert!(output.contains("\x1b8"), "should contain restore cursor");
    }

    #[test]
    fn test_render_positions_at_layout() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions();
        let state = OverlayState::new();
        render_popup(&mut buf, &suggestions, &state, 5, 0, 24, 80);
        let output = String::from_utf8_lossy(&buf);
        // Popup below cursor at row 5 → starts at row 6 (1-indexed: 7)
        assert!(
            output.contains("\x1b[7;1H"),
            "should position at row 7 col 1"
        );
    }

    #[test]
    fn test_selected_item_has_reverse_video() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions();
        let state = OverlayState::new(); // selected = 0
        render_popup(&mut buf, &suggestions, &state, 5, 0, 24, 80);
        let output = String::from_utf8_lossy(&buf);
        assert!(
            output.contains("\x1b[7m"),
            "should contain reverse video for selected"
        );
    }

    #[test]
    fn test_format_item_shows_kind_gutter() {
        let mut buf = Vec::new();
        let s = make("checkout", None, SuggestionKind::Subcommand);
        format_item(&mut buf, &s, 30, false);
        let output = String::from_utf8_lossy(&buf);
        assert!(
            output.starts_with(" S checkout"),
            "should show kind char S for subcommand: got '{output}'"
        );
    }

    #[test]
    fn test_format_item_truncates_description() {
        let mut buf = Vec::new();
        let long_desc = "a".repeat(200);
        let s = make("cmd", Some(&long_desc), SuggestionKind::Command);
        format_item(&mut buf, &s, 30, false);
        // Output should not exceed width
        // (hard to check exact byte count due to ANSI codes, but it shouldn't be huge)
        assert!(buf.len() < 200, "should truncate description");
    }

    #[test]
    fn test_clear_writes_spaces() {
        let mut buf = Vec::new();
        let layout = PopupLayout {
            start_row: 5,
            start_col: 0,
            width: 20,
            height: 3,
            renders_above: false,
        };
        clear_popup(&mut buf, &layout);
        let output = String::from_utf8_lossy(&buf);
        // Should NOT contain erase-to-EOL
        assert!(!output.contains("\x1b[K"), "should not use erase_to_eol");
        // Should contain lots of spaces
        assert!(
            output.contains("                    "),
            "should write spaces"
        );
    }

    #[test]
    fn test_clear_correct_dimensions() {
        let mut buf = Vec::new();
        let layout = PopupLayout {
            start_row: 10,
            start_col: 5,
            width: 25,
            height: 4,
            renders_above: false,
        };
        clear_popup(&mut buf, &layout);
        let output = String::from_utf8_lossy(&buf);
        // Should have cursor moves to 4 rows
        assert!(output.contains("\x1b[11;6H"), "row 10 -> ANSI row 11");
        assert!(output.contains("\x1b[12;6H"), "row 11 -> ANSI row 12");
        assert!(output.contains("\x1b[13;6H"), "row 12 -> ANSI row 13");
        assert!(output.contains("\x1b[14;6H"), "row 13 -> ANSI row 14");
    }

    #[test]
    fn test_render_with_scroll_offset() {
        let mut buf = Vec::new();
        let suggestions: Vec<Suggestion> = (0..15)
            .map(|i| make(&format!("item{i}"), None, SuggestionKind::Command))
            .collect();
        let mut state = OverlayState::new();
        state.scroll_offset = 5;
        state.selected = 5;
        let layout = render_popup(&mut buf, &suggestions, &state, 5, 0, 24, 80);
        let output = String::from_utf8_lossy(&buf);
        // Should show item5 (first visible after offset), not item0
        assert!(
            output.contains("item5"),
            "should show item5 at scroll_offset=5"
        );
        assert!(
            !output.contains("item0"),
            "should not show item0 when scrolled"
        );
        assert_eq!(layout.height, 10); // MAX_VISIBLE
    }

    #[test]
    fn test_render_empty_suggestions() {
        let mut buf = Vec::new();
        let suggestions: Vec<Suggestion> = vec![];
        let state = OverlayState::new();
        let layout = render_popup(&mut buf, &suggestions, &state, 5, 0, 24, 80);
        assert_eq!(layout.height, 0);
        assert!(
            buf.is_empty(),
            "should produce no output for empty suggestions"
        );
    }
}
