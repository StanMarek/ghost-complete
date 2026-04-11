//! Intermediate popup frame representation.
//!
//! `PopupFrame` captures the visual content of a popup (rows, styled spans,
//! scrollbar indicators) without any ANSI escape sequences. This allows the
//! same popup content logic to be rendered via:
//! - ANSI escape sequences (real proxy popup — existing render.rs path)
//! - ratatui widgets (TUI config editor preview)
//!
//! **Design decision:** This module exists *alongside* render.rs, not as a
//! replacement. render_popup() is 2325 lines with 115 tests and subtle ANSI
//! byte-level state transitions. Rewiring it through a frame model is
//! high-risk for zero user-visible benefit. Instead, frame.rs reuses the
//! same pure helpers (kind_icon, sanitize_display_text,
//! translate_match_indices) and implements parallel content construction.

use gc_suggest::Suggestion;

use crate::layout::{DESC_GAP_COLS, GUTTER_COLS, TRAILING_PAD_COLS};
use crate::render::{kind_icon, sanitize_display_text, translate_match_indices};
use crate::types::{OverlayState, PopupLayout};
use crate::util::display_text;

/// Abstract style role applied to a text span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanStyle {
    /// Default item text.
    Plain,
    /// Fuzzy-match highlighted character.
    MatchHighlight,
    /// Description text (dim).
    Description,
    /// Gutter icon + padding.
    Gutter,
    /// Border characters.
    Border,
    /// Scrollbar characters.
    Scrollbar,
    /// Loading indicator.
    Loading,
}

/// A run of text sharing the same style within a popup row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledSpan {
    pub text: String,
    pub style: SpanStyle,
}

/// Scrollbar indicator for a content row's right edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollbarCell {
    /// No scrollbar needed (all items fit).
    None,
    /// Scrollbar thumb (filled block).
    Thumb,
    /// Scrollbar track (dotted line).
    Track,
}

/// A content row representing one suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentRow {
    pub is_selected: bool,
    pub spans: Vec<StyledSpan>,
    pub scrollbar: ScrollbarCell,
}

/// A single row in the popup frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PopupRow {
    /// Top or bottom border (e.g., "╭──╮" or "╰──╯").
    Border { text: String },
    /// A suggestion content row.
    Content(ContentRow),
    /// Loading indicator ("  ...").
    Loading { spans: Vec<StyledSpan> },
}

/// Complete popup frame ready for rendering. Pure data — no ANSI escapes.
#[derive(Debug, Clone)]
pub struct PopupFrame {
    pub rows: Vec<PopupRow>,
    pub borders: bool,
    pub content_width: u16,
    pub total_width: u16,
}

// ---------------------------------------------------------------------------
// Builder functions
// ---------------------------------------------------------------------------

/// Walk `text` char-by-char, segmenting into runs of Plain vs MatchHighlight
/// based on whether the char's index appears in `match_indices`. Stops when
/// the next character would exceed `max_cols` display columns.
///
/// Returns `(spans, cols_written)`.
pub fn segment_highlighted_text(
    text: &str,
    match_indices: &[u32],
    max_cols: usize,
) -> (Vec<StyledSpan>, usize) {
    let mut spans: Vec<StyledSpan> = Vec::new();
    let mut cols_written: usize = 0;

    for (char_idx, ch) in text.chars().enumerate() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if cols_written + ch_width > max_cols {
            break;
        }

        let is_match = match_indices.binary_search(&(char_idx as u32)).is_ok();
        let style = if is_match {
            SpanStyle::MatchHighlight
        } else {
            SpanStyle::Plain
        };

        // Extend the last span if the style matches, otherwise start a new one.
        match spans.last_mut() {
            Some(last) if last.style == style => {
                last.text.push(ch);
            }
            _ => {
                spans.push(StyledSpan {
                    text: String::from(ch),
                    style,
                });
            }
        }

        cols_written += ch_width;
    }

    (spans, cols_written)
}

/// Build one content row for a suggestion.
///
/// Layout: `" K " text [  description] trailing_pad [scrollbar]`
pub fn build_content_row(
    s: &Suggestion,
    item_width: u16,
    is_selected: bool,
    scrollbar: ScrollbarCell,
) -> ContentRow {
    let mut spans: Vec<StyledSpan> = Vec::new();
    let total_width = item_width as usize;

    // 1. Gutter: " K "
    let icon = kind_icon(s.kind);
    spans.push(StyledSpan {
        text: format!(" {icon} "),
        style: SpanStyle::Gutter,
    });

    // 2. Text with match highlighting
    let max_text_cols = total_width.saturating_sub(GUTTER_COLS);
    let (raw_display_text, prefix_char_count) = display_text(s);
    let sanitized = sanitize_display_text(raw_display_text);
    let match_indices = translate_match_indices(
        raw_display_text,
        &sanitized,
        prefix_char_count,
        &s.match_indices,
    );
    let (text_spans, cols_written) =
        segment_highlighted_text(&sanitized, &match_indices, max_text_cols);
    spans.extend(text_spans);

    let gutter_text_len = GUTTER_COLS + cols_written;

    // 3. Description (if present and room available)
    let desc_sanitized = s
        .description
        .as_deref()
        .map(sanitize_display_text)
        .unwrap_or_default();
    let max_desc_cols =
        total_width.saturating_sub(gutter_text_len + DESC_GAP_COLS + TRAILING_PAD_COLS);

    if !desc_sanitized.is_empty() && max_desc_cols > 2 {
        // Gap padding
        spans.push(StyledSpan {
            text: " ".repeat(DESC_GAP_COLS),
            style: SpanStyle::Plain,
        });

        // Truncate description by display columns
        let mut desc_cols: usize = 0;
        let mut truncated = String::new();
        for ch in desc_sanitized.chars() {
            let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if desc_cols + w > max_desc_cols {
                break;
            }
            truncated.push(ch);
            desc_cols += w;
        }

        let desc_style = if is_selected {
            SpanStyle::Plain
        } else {
            SpanStyle::Description
        };
        spans.push(StyledSpan {
            text: truncated,
            style: desc_style,
        });

        // Trailing padding to fill row
        let remaining =
            total_width.saturating_sub(gutter_text_len + DESC_GAP_COLS + desc_cols);
        if remaining > 0 {
            spans.push(StyledSpan {
                text: " ".repeat(remaining),
                style: SpanStyle::Plain,
            });
        }
    } else {
        // No description or no room: trailing padding
        let remaining = total_width.saturating_sub(gutter_text_len);
        if remaining > 0 {
            spans.push(StyledSpan {
                text: " ".repeat(remaining),
                style: SpanStyle::Plain,
            });
        }
    }

    ContentRow {
        is_selected,
        spans,
        scrollbar,
    }
}

/// Build the complete popup frame from suggestions, overlay state, and layout.
///
/// Returns `None` if there are no suggestions or the layout has zero height.
pub fn build_popup_frame(
    suggestions: &[Suggestion],
    state: &OverlayState,
    layout: &PopupLayout,
    effective_max: usize,
    borders: bool,
    loading: bool,
) -> Option<PopupFrame> {
    if suggestions.is_empty() || layout.height == 0 {
        return None;
    }

    let border_pad: u16 = if borders { 2 } else { 0 };
    let content_width = layout.width.saturating_sub(border_pad);
    let content_height = layout.height.saturating_sub(border_pad);

    // Scrollbar computation (mirrors render.rs algorithm)
    let needs_scrollbar = suggestions.len() > effective_max;
    let (thumb_pos, thumb_size) = if needs_scrollbar {
        let total = suggestions.len();
        let visible = content_height as usize;
        let ts = std::cmp::max(1, visible * visible / total).min(visible);
        let tp = if total > visible {
            (state.scroll_offset * (visible - ts) / (total - visible))
                .min(visible.saturating_sub(ts))
        } else {
            0
        };
        (tp, ts)
    } else {
        (0, 0)
    };

    let item_width = if needs_scrollbar {
        content_width.saturating_sub(1)
    } else {
        content_width
    };

    let mut rows: Vec<PopupRow> = Vec::new();

    // Top border
    if borders {
        let inner = "─".repeat(content_width as usize);
        rows.push(PopupRow::Border {
            text: format!("╭{inner}╮"),
        });
    }

    // Content rows from visible slice
    let end = (state.scroll_offset + content_height as usize).min(suggestions.len());
    let visible_slice = &suggestions[state.scroll_offset..end];

    for (i, suggestion) in visible_slice.iter().enumerate() {
        let is_selected = state.selected == Some(state.scroll_offset + i);

        let scrollbar = if needs_scrollbar {
            if i >= thumb_pos && i < thumb_pos + thumb_size {
                ScrollbarCell::Thumb
            } else {
                ScrollbarCell::Track
            }
        } else {
            ScrollbarCell::None
        };

        rows.push(PopupRow::Content(build_content_row(
            suggestion,
            item_width,
            is_selected,
            scrollbar,
        )));
    }

    // Loading row
    if loading {
        let label = "  ...";
        let pad_len = (content_width as usize).saturating_sub(label.len());
        let mut loading_spans = vec![StyledSpan {
            text: label.to_string(),
            style: SpanStyle::Loading,
        }];
        if pad_len > 0 {
            loading_spans.push(StyledSpan {
                text: " ".repeat(pad_len),
                style: SpanStyle::Loading,
            });
        }
        rows.push(PopupRow::Loading {
            spans: loading_spans,
        });
    }

    // Bottom border
    if borders {
        let inner = "─".repeat(content_width as usize);
        rows.push(PopupRow::Border {
            text: format!("╰{inner}╯"),
        });
    }

    Some(PopupFrame {
        rows,
        borders,
        content_width,
        total_width: layout.width,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gc_suggest::{SuggestionKind, SuggestionSource};

    fn make(text: &str, desc: Option<&str>, kind: SuggestionKind) -> Suggestion {
        Suggestion {
            text: text.to_string(),
            description: desc.map(String::from),
            kind,
            source: SuggestionSource::Spec,
            ..Default::default()
        }
    }

    fn make_with_matches(
        text: &str,
        desc: Option<&str>,
        kind: SuggestionKind,
        indices: Vec<u32>,
    ) -> Suggestion {
        Suggestion {
            text: text.to_string(),
            description: desc.map(String::from),
            kind,
            match_indices: indices,
            source: SuggestionSource::Spec,
            ..Default::default()
        }
    }

    // -----------------------------------------------------------------------
    // segment_highlighted_text
    // -----------------------------------------------------------------------

    #[test]
    fn segment_no_highlights() {
        let (spans, cols) = segment_highlighted_text("hello", &[], 20);
        assert_eq!(cols, 5);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style, SpanStyle::Plain);
        assert_eq!(spans[0].text, "hello");
    }

    #[test]
    fn segment_with_highlights() {
        // Highlight chars at indices 1 and 3: "hElLo"
        let (spans, cols) = segment_highlighted_text("hello", &[1, 3], 20);
        assert_eq!(cols, 5);
        // Runs: Plain("h"), Highlight("e"), Plain("l"), Highlight("l"), Plain("o")
        assert_eq!(spans.len(), 5);
        assert_eq!(spans[0], StyledSpan { text: "h".into(), style: SpanStyle::Plain });
        assert_eq!(spans[1], StyledSpan { text: "e".into(), style: SpanStyle::MatchHighlight });
        assert_eq!(spans[2], StyledSpan { text: "l".into(), style: SpanStyle::Plain });
        assert_eq!(spans[3], StyledSpan { text: "l".into(), style: SpanStyle::MatchHighlight });
        assert_eq!(spans[4], StyledSpan { text: "o".into(), style: SpanStyle::Plain });
    }

    #[test]
    fn segment_consecutive_highlights_merge() {
        // Highlight chars 0, 1, 2 consecutively
        let (spans, cols) = segment_highlighted_text("abc", &[0, 1, 2], 20);
        assert_eq!(cols, 3);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style, SpanStyle::MatchHighlight);
        assert_eq!(spans[0].text, "abc");
    }

    #[test]
    fn segment_truncation_at_max_cols() {
        let (spans, cols) = segment_highlighted_text("hello world", &[], 5);
        assert_eq!(cols, 5);
        let combined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(combined, "hello");
    }

    #[test]
    fn segment_empty_text() {
        let (spans, cols) = segment_highlighted_text("", &[], 20);
        assert_eq!(cols, 0);
        assert!(spans.is_empty());
    }

    #[test]
    fn segment_cjk_width_counting() {
        // Each CJK char is 2 display columns
        let (spans, cols) = segment_highlighted_text("\u{65E5}\u{672C}\u{8A9E}", &[], 5);
        // Only 2 chars fit (4 cols), 3rd would be 6 cols > 5
        assert_eq!(cols, 4);
        let combined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(combined, "\u{65E5}\u{672C}");
    }

    #[test]
    fn segment_zero_max_cols() {
        let (spans, cols) = segment_highlighted_text("hello", &[], 0);
        assert_eq!(cols, 0);
        assert!(spans.is_empty());
    }

    // -----------------------------------------------------------------------
    // build_content_row
    // -----------------------------------------------------------------------

    #[test]
    fn content_row_basic_with_description() {
        let s = make("checkout", Some("Switch branches"), SuggestionKind::Subcommand);
        let row = build_content_row(&s, 40, false, ScrollbarCell::None);
        assert!(!row.is_selected);
        assert_eq!(row.scrollbar, ScrollbarCell::None);

        // First span should be gutter
        assert_eq!(row.spans[0].style, SpanStyle::Gutter);
        assert!(row.spans[0].text.contains(kind_icon(SuggestionKind::Subcommand)));

        // Should contain description span with Description style
        let has_desc = row.spans.iter().any(|sp| sp.style == SpanStyle::Description);
        assert!(has_desc, "should have a Description-styled span");

        // Description text should appear
        let full_text: String = row.spans.iter().map(|s| s.text.as_str()).collect();
        assert!(
            full_text.contains("Switch branches"),
            "description text should appear in row: {full_text}"
        );
    }

    #[test]
    fn content_row_selected_desc_uses_plain() {
        let s = make("checkout", Some("Switch branches"), SuggestionKind::Subcommand);
        let row = build_content_row(&s, 40, true, ScrollbarCell::None);
        assert!(row.is_selected);

        // When selected, description should use Plain style, not Description
        let has_desc_style = row.spans.iter().any(|sp| sp.style == SpanStyle::Description);
        assert!(
            !has_desc_style,
            "selected row should not have Description style"
        );
    }

    #[test]
    fn content_row_filepath_basename() {
        let s = make("src/main.rs", None, SuggestionKind::FilePath);
        let row = build_content_row(&s, 30, false, ScrollbarCell::None);
        let full_text: String = row.spans.iter().map(|s| s.text.as_str()).collect();

        // Should contain "main.rs" (basename), not "src/"
        assert!(
            full_text.contains("main.rs"),
            "should show basename: {full_text}"
        );
        // The gutter + text portion should not contain the prefix "src/"
        // (the gutter has icon, not "src/")
        let text_spans: String = row
            .spans
            .iter()
            .filter(|sp| sp.style == SpanStyle::Plain || sp.style == SpanStyle::MatchHighlight)
            .map(|sp| sp.text.as_str())
            .collect();
        assert!(
            !text_spans.starts_with("src/"),
            "should not show path prefix: {text_spans}"
        );
    }

    #[test]
    fn content_row_match_highlighting() {
        // "checkout": c(0) h(1) e(2) c(3) k(4) o(5) u(6) t(7)
        // Match indices 0 and 6 -> 'c' and 'u'
        let s = make_with_matches("checkout", None, SuggestionKind::Command, vec![0, 6]);
        let row = build_content_row(&s, 30, false, ScrollbarCell::None);

        let highlight_spans: Vec<&StyledSpan> = row
            .spans
            .iter()
            .filter(|sp| sp.style == SpanStyle::MatchHighlight)
            .collect();
        assert!(
            !highlight_spans.is_empty(),
            "should have match highlight spans"
        );
        // 'c' and 'u' should be highlighted
        let highlighted_chars: String = highlight_spans.iter().map(|s| s.text.as_str()).collect();
        assert!(highlighted_chars.contains('c'), "should highlight 'c'");
        assert!(highlighted_chars.contains('u'), "should highlight 'u'");
    }

    #[test]
    fn content_row_no_description_narrow() {
        // Very narrow width: no room for description
        let s = make("ls", Some("List directory contents"), SuggestionKind::Command);
        // Width 8: GUTTER(4) + "ls"(2) + trailing(1) = 7, only 1 col left for desc
        let row = build_content_row(&s, 8, false, ScrollbarCell::None);
        let has_desc = row.spans.iter().any(|sp| sp.style == SpanStyle::Description);
        assert!(!has_desc, "too narrow for description");
    }

    #[test]
    fn content_row_scrollbar_cell() {
        let s = make("item", None, SuggestionKind::Command);
        let row = build_content_row(&s, 20, false, ScrollbarCell::Thumb);
        assert_eq!(row.scrollbar, ScrollbarCell::Thumb);
    }

    // -----------------------------------------------------------------------
    // build_popup_frame
    // -----------------------------------------------------------------------

    #[test]
    fn frame_empty_returns_none() {
        let state = OverlayState::new();
        let layout = PopupLayout {
            start_row: 1,
            start_col: 0,
            width: 30,
            height: 5,
            scroll_deficit: 0,
        };
        let result = build_popup_frame(&[], &state, &layout, 10, true, false);
        assert!(result.is_none());
    }

    #[test]
    fn frame_zero_height_returns_none() {
        let suggestions = vec![make("ls", None, SuggestionKind::Command)];
        let state = OverlayState::new();
        let layout = PopupLayout {
            start_row: 1,
            start_col: 0,
            width: 30,
            height: 0,
            scroll_deficit: 0,
        };
        let result = build_popup_frame(&suggestions, &state, &layout, 10, true, false);
        assert!(result.is_none());
    }

    #[test]
    fn frame_with_borders() {
        let suggestions = vec![
            make("checkout", Some("Switch branches"), SuggestionKind::Subcommand),
            make("commit", Some("Record changes"), SuggestionKind::Subcommand),
        ];
        let state = OverlayState::new();
        let layout = PopupLayout {
            start_row: 1,
            start_col: 0,
            width: 40,
            height: 4, // 2 content + 2 borders
            scroll_deficit: 0,
        };
        let frame = build_popup_frame(&suggestions, &state, &layout, 10, true, false).unwrap();
        assert!(frame.borders);
        assert_eq!(frame.total_width, 40);
        assert_eq!(frame.content_width, 38); // 40 - 2 borders

        // Should have: top border, 2 content, bottom border = 4 rows
        assert_eq!(frame.rows.len(), 4);
        assert!(matches!(frame.rows[0], PopupRow::Border { .. }));
        assert!(matches!(frame.rows[1], PopupRow::Content(_)));
        assert!(matches!(frame.rows[2], PopupRow::Content(_)));
        assert!(matches!(frame.rows[3], PopupRow::Border { .. }));

        // Check border text
        if let PopupRow::Border { ref text } = frame.rows[0] {
            assert!(text.starts_with('╭'));
            assert!(text.ends_with('╮'));
            assert!(text.contains('─'));
        }
        if let PopupRow::Border { ref text } = frame.rows[3] {
            assert!(text.starts_with('╰'));
            assert!(text.ends_with('╯'));
        }
    }

    #[test]
    fn frame_without_borders() {
        let suggestions = vec![
            make("ls", None, SuggestionKind::Command),
            make("cd", None, SuggestionKind::Command),
        ];
        let state = OverlayState::new();
        let layout = PopupLayout {
            start_row: 1,
            start_col: 0,
            width: 30,
            height: 2, // 2 content, no borders
            scroll_deficit: 0,
        };
        let frame = build_popup_frame(&suggestions, &state, &layout, 10, false, false).unwrap();
        assert!(!frame.borders);
        assert_eq!(frame.content_width, 30);
        assert_eq!(frame.rows.len(), 2);
        assert!(matches!(frame.rows[0], PopupRow::Content(_)));
        assert!(matches!(frame.rows[1], PopupRow::Content(_)));
    }

    #[test]
    fn frame_with_loading() {
        let suggestions = vec![make("ls", None, SuggestionKind::Command)];
        let state = OverlayState::new();
        let layout = PopupLayout {
            start_row: 1,
            start_col: 0,
            width: 30,
            height: 3, // 1 content + 2 borders
            scroll_deficit: 0,
        };
        let frame = build_popup_frame(&suggestions, &state, &layout, 10, true, true).unwrap();

        // Should have: top border, 1 content, loading, bottom border = 4 rows
        assert_eq!(frame.rows.len(), 4);
        assert!(matches!(frame.rows[0], PopupRow::Border { .. }));
        assert!(matches!(frame.rows[1], PopupRow::Content(_)));
        assert!(matches!(frame.rows[2], PopupRow::Loading { .. }));
        assert!(matches!(frame.rows[3], PopupRow::Border { .. }));

        // Loading row should contain "  ..."
        if let PopupRow::Loading { ref spans } = frame.rows[2] {
            let text: String = spans.iter().map(|s| s.text.as_str()).collect();
            assert!(text.contains("..."), "loading row should contain '...': {text}");
            assert!(
                spans.iter().all(|s| s.style == SpanStyle::Loading),
                "all loading spans should have Loading style"
            );
        }
    }

    #[test]
    fn frame_with_selection() {
        let suggestions = vec![
            make("checkout", None, SuggestionKind::Subcommand),
            make("commit", None, SuggestionKind::Subcommand),
            make("cherry-pick", None, SuggestionKind::Subcommand),
        ];
        let mut state = OverlayState::new();
        state.selected = Some(1); // select "commit"
        let layout = PopupLayout {
            start_row: 1,
            start_col: 0,
            width: 30,
            height: 3, // 3 content, no borders
            scroll_deficit: 0,
        };
        let frame = build_popup_frame(&suggestions, &state, &layout, 10, false, false).unwrap();
        assert_eq!(frame.rows.len(), 3);

        // Row 0 should not be selected, row 1 should be selected
        if let PopupRow::Content(ref row) = frame.rows[0] {
            assert!(!row.is_selected);
        }
        if let PopupRow::Content(ref row) = frame.rows[1] {
            assert!(row.is_selected);
        }
        if let PopupRow::Content(ref row) = frame.rows[2] {
            assert!(!row.is_selected);
        }
    }

    #[test]
    fn frame_with_scrollbar() {
        // 20 suggestions, effective_max = 5 -> needs scrollbar
        let suggestions: Vec<Suggestion> = (0..20)
            .map(|i| make(&format!("item{i}"), None, SuggestionKind::Command))
            .collect();
        let state = OverlayState::new();
        let layout = PopupLayout {
            start_row: 1,
            start_col: 0,
            width: 30,
            height: 5, // 5 content, no borders
            scroll_deficit: 0,
        };
        let frame = build_popup_frame(&suggestions, &state, &layout, 5, false, false).unwrap();
        assert_eq!(frame.rows.len(), 5);

        // At least one row should have Thumb and at least one Track
        let mut has_thumb = false;
        let mut has_track = false;
        for row in &frame.rows {
            if let PopupRow::Content(ref cr) = row {
                match cr.scrollbar {
                    ScrollbarCell::Thumb => has_thumb = true,
                    ScrollbarCell::Track => has_track = true,
                    ScrollbarCell::None => {}
                }
            }
        }
        assert!(has_thumb, "scrollbar should have at least one thumb cell");
        assert!(has_track, "scrollbar should have at least one track cell");
    }

    #[test]
    fn frame_no_scrollbar_when_fits() {
        let suggestions = vec![
            make("ls", None, SuggestionKind::Command),
            make("cd", None, SuggestionKind::Command),
        ];
        let state = OverlayState::new();
        let layout = PopupLayout {
            start_row: 1,
            start_col: 0,
            width: 30,
            height: 2,
            scroll_deficit: 0,
        };
        let frame = build_popup_frame(&suggestions, &state, &layout, 10, false, false).unwrap();
        for row in &frame.rows {
            if let PopupRow::Content(ref cr) = row {
                assert_eq!(
                    cr.scrollbar,
                    ScrollbarCell::None,
                    "no scrollbar when all items fit"
                );
            }
        }
    }

    #[test]
    fn frame_border_width_matches_content() {
        let suggestions = vec![make("test", None, SuggestionKind::Command)];
        let state = OverlayState::new();
        let layout = PopupLayout {
            start_row: 1,
            start_col: 0,
            width: 25,
            height: 3, // 1 content + 2 borders
            scroll_deficit: 0,
        };
        let frame = build_popup_frame(&suggestions, &state, &layout, 10, true, false).unwrap();
        // content_width = 25 - 2 = 23
        assert_eq!(frame.content_width, 23);
        if let PopupRow::Border { ref text } = frame.rows[0] {
            // Border should be: ╭ + 23*─ + ╮ = corner(1) + inner(23) + corner(1)
            let dash_count = text.chars().filter(|c| *c == '─').count();
            assert_eq!(dash_count, 23, "border dashes should match content_width");
        }
    }

    #[test]
    fn frame_scroll_offset_slices_correctly() {
        let suggestions: Vec<Suggestion> = (0..10)
            .map(|i| make(&format!("item{i}"), None, SuggestionKind::Command))
            .collect();
        let mut state = OverlayState::new();
        state.scroll_offset = 3;
        state.selected = Some(3);
        let layout = PopupLayout {
            start_row: 1,
            start_col: 0,
            width: 30,
            height: 5, // 5 content, no borders
            scroll_deficit: 0,
        };
        let frame = build_popup_frame(&suggestions, &state, &layout, 5, false, false).unwrap();
        assert_eq!(frame.rows.len(), 5);

        // First visible row should be selected (index 3 = scroll_offset)
        if let PopupRow::Content(ref cr) = frame.rows[0] {
            assert!(cr.is_selected, "first visible row should be selected");
        }
    }
}

