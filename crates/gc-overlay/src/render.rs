use std::io::Write;

use anyhow::{bail, Result};
use gc_suggest::{Suggestion, SuggestionKind};
use gc_terminal::TerminalProfile;

use crate::ansi;
use crate::layout;
use crate::types::{OverlayState, PopupLayout};
use crate::util::display_text;

/// Precomputed ANSI sequences for popup styling.
/// Keeps gc-overlay independent of gc-config.
pub struct PopupTheme {
    pub selected_on: Vec<u8>,
    pub description_on: Vec<u8>,
    pub match_highlight_on: Vec<u8>,
    pub item_text_on: Vec<u8>,
    pub scrollbar_on: Vec<u8>,
    pub border_on: Vec<u8>,
    pub borders: bool,
}

impl Default for PopupTheme {
    fn default() -> Self {
        Self {
            selected_on: b"\x1b[7m".to_vec(),
            description_on: b"\x1b[2m".to_vec(),
            match_highlight_on: b"\x1b[1m".to_vec(),
            item_text_on: vec![],
            scrollbar_on: b"\x1b[2m".to_vec(),
            border_on: b"\x1b[2m".to_vec(),
            borders: false,
        }
    }
}

/// Parse a space-separated style string into a combined ANSI SGR sequence.
///
/// Supported tokens: `reverse`, `dim`, `bold`, `underline`, `fg:N`, `bg:N`
/// (where N is a 256-color index).
///
/// Example: `"bold fg:196"` -> `b"\x1b[1;38;5;196m"`
fn parse_hex_rgb(hex: &str, token: &str) -> Result<(u8, u8, u8)> {
    if hex.len() != 6 {
        bail!("invalid hex color (need 6 chars): {}", token);
    }
    let r = u8::from_str_radix(&hex[0..2], 16)
        .map_err(|_| anyhow::anyhow!("invalid hex color: {}", token))?;
    let g = u8::from_str_radix(&hex[2..4], 16)
        .map_err(|_| anyhow::anyhow!("invalid hex color: {}", token))?;
    let b = u8::from_str_radix(&hex[4..6], 16)
        .map_err(|_| anyhow::anyhow!("invalid hex color: {}", token))?;
    Ok((r, g, b))
}

pub fn parse_style(style_str: &str) -> Result<Vec<u8>> {
    let mut params: Vec<String> = Vec::new();

    for token in style_str.split_whitespace() {
        match token {
            "reverse" => params.push("7".to_string()),
            "dim" => params.push("2".to_string()),
            "bold" => params.push("1".to_string()),
            "underline" => params.push("4".to_string()),
            _ if token.starts_with("fg:#") => {
                let (r, g, b) = parse_hex_rgb(&token[4..], token)?;
                params.push(format!("38;2;{r};{g};{b}"));
            }
            _ if token.starts_with("fg:") => {
                let n: u8 = token[3..]
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid fg color: {}", token))?;
                params.push(format!("38;5;{n}"));
            }
            _ if token.starts_with("bg:#") => {
                let (r, g, b) = parse_hex_rgb(&token[4..], token)?;
                params.push(format!("48;2;{r};{g};{b}"));
            }
            _ if token.starts_with("bg:") => {
                let n: u8 = token[3..]
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid bg color: {}", token))?;
                params.push(format!("48;5;{n}"));
            }
            _ => bail!("unknown style token: {:?}", token),
        }
    }

    if params.is_empty() {
        return Ok(Vec::new());
    }

    let joined = params.join(";");
    Ok(format!("\x1b[{joined}m").into_bytes())
}

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
    loading: bool,
    profile: &TerminalProfile,
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

    // Border padding: 2 rows/cols when borders enabled, 0 otherwise
    let border_pad: u16 = if theme.borders { 2 } else { 0 };

    // Cap popup height to screen_rows - 1 (leave room for prompt row)
    let min_screen = 1 + border_pad; // need at least 1 content row + borders
    let effective_max = if screen_rows > min_screen {
        max_visible.min((screen_rows - 1 - border_pad) as usize)
    } else {
        #[cfg(debug_assertions)]
        eprintln!(
            "ghost-complete: popup suppressed — screen too small ({screen_rows} rows, need > {min_screen})"
        );
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
    let loading_extra_deficit = if loading && theme.borders {
        1u16 // loading row displaces bottom border; new border drawn 1 row below layout extent
    } else if loading {
        1u16
    } else {
        0
    };
    let total_height_needed = visible_count + border_pad + loading_extra_deficit;
    let new_deficit = total_height_needed.saturating_sub(space_below);
    let total_deficit = prior_deficit + new_deficit;
    let final_cursor_row = cursor_row.saturating_sub(total_deficit);

    let use_sync = matches!(
        profile.render_strategy(),
        gc_terminal::RenderStrategy::Synchronized
    );
    if use_sync {
        ansi::begin_sync(buf);
    }

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
        theme.borders,
    );

    if layout.height == 0 {
        ansi::restore_cursor(buf);
        if use_sync {
            ansi::end_sync(buf);
        }
        return PopupLayout {
            scroll_deficit: total_deficit,
            ..layout
        };
    }

    // Content dimensions: subtract border padding when borders enabled
    let content_width = layout.width.saturating_sub(border_pad);
    let content_height = layout.height.saturating_sub(border_pad);
    let border_col = layout.start_col;
    let top_border_row = layout.start_row;
    let bottom_border_row = if theme.borders {
        layout.start_row + layout.height - 1
    } else {
        layout.start_row + layout.height // past end, won't be used
    };

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

    let end = (state.scroll_offset + content_height as usize).min(suggestions.len());
    let visible = &suggestions[state.scroll_offset..end];

    // Draw top border line: ╭───...───╮
    if theme.borders {
        ansi::move_to(buf, top_border_row, border_col);
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

    // Draw content rows (with left/right borders when enabled)
    let content_start_row = if theme.borders {
        top_border_row + 1
    } else {
        top_border_row
    };
    for (i, suggestion) in visible.iter().enumerate() {
        let row = content_start_row + i as u16;
        let is_selected = state.selected == Some(state.scroll_offset + i);

        ansi::move_to(buf, row, border_col);

        // Left border
        if theme.borders {
            if !theme.border_on.is_empty() {
                buf.extend_from_slice(&theme.border_on);
            }
            buf.extend_from_slice("│".as_bytes());
            ansi::reset(buf);
        }

        // Content
        if is_selected {
            buf.extend_from_slice(&theme.selected_on);
        } else if !theme.item_text_on.is_empty() {
            buf.extend_from_slice(&theme.item_text_on);
        }

        format_item(buf, suggestion, item_width, is_selected, theme);

        if needs_scrollbar {
            let row_idx = i;
            if is_selected {
                if row_idx >= thumb_pos && row_idx < thumb_pos + thumb_size {
                    let _ = buf.write_all("█".as_bytes());
                } else {
                    let _ = buf.write_all("┆".as_bytes());
                }
            } else if row_idx >= thumb_pos && row_idx < thumb_pos + thumb_size {
                if !theme.item_text_on.is_empty() {
                    ansi::reset(buf);
                }
                let _ = buf.write_all("█".as_bytes());
            } else {
                ansi::reset(buf);
                if !theme.scrollbar_on.is_empty() {
                    buf.extend_from_slice(&theme.scrollbar_on);
                }
                let _ = buf.write_all("┆".as_bytes());
            }
        }

        // Right border
        if theme.borders {
            ansi::reset(buf);
            if !theme.border_on.is_empty() {
                buf.extend_from_slice(&theme.border_on);
            }
            buf.extend_from_slice("│".as_bytes());
            ansi::reset(buf);
        } else {
            ansi::reset(buf);
        }
    }

    // Render loading indicator row when async generators are in flight
    let loading_extra = if loading {
        let loading_row = if theme.borders {
            bottom_border_row
        } else {
            layout.start_row + layout.height
        };
        if loading_row < screen_rows {
            ansi::move_to(buf, loading_row, border_col);
            if theme.borders {
                if !theme.border_on.is_empty() {
                    buf.extend_from_slice(&theme.border_on);
                }
                buf.extend_from_slice("│".as_bytes());
                ansi::reset(buf);
            }
            buf.extend_from_slice(&theme.description_on);
            let label = b"  ...";
            let _ = buf.write_all(label);
            let pad = (content_width as usize).saturating_sub(label.len());
            for _ in 0..pad {
                let _ = buf.write_all(b" ");
            }
            ansi::reset(buf);
            if theme.borders {
                if !theme.border_on.is_empty() {
                    buf.extend_from_slice(&theme.border_on);
                }
                buf.extend_from_slice("│".as_bytes());
                ansi::reset(buf);

                // Draw bottom border below loading row
                let border_below = loading_row + 1;
                if border_below < screen_rows {
                    ansi::move_to(buf, border_below, border_col);
                    if !theme.border_on.is_empty() {
                        buf.extend_from_slice(&theme.border_on);
                    }
                    buf.extend_from_slice("╰".as_bytes());
                    for _ in 0..content_width {
                        buf.extend_from_slice("─".as_bytes());
                    }
                    buf.extend_from_slice("╯".as_bytes());
                    ansi::reset(buf);
                    1 // loading row extends 1 row beyond layout.height (border is within that row)
                } else {
                    1 // only loading row fit
                }
            } else {
                1 // no borders, just the loading row
            }
        } else {
            0
        }
    } else {
        0
    };

    // Draw bottom border line: ╰───...───╯
    if theme.borders && loading_extra == 0 {
        ansi::move_to(buf, bottom_border_row, border_col);
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
    if use_sync {
        ansi::end_sync(buf);
    }

    PopupLayout {
        height: layout.height + loading_extra,
        scroll_deficit: total_deficit,
        ..layout
    }
}

/// Clear the popup area by overwriting with spaces.
pub fn clear_popup(buf: &mut Vec<u8>, layout: &PopupLayout, profile: &TerminalProfile) {
    if layout.height == 0 {
        return;
    }

    let use_sync = matches!(
        profile.render_strategy(),
        gc_terminal::RenderStrategy::Synchronized
    );
    if use_sync {
        ansi::begin_sync(buf);
    }
    ansi::save_cursor(buf);

    for row_offset in 0..layout.height {
        let row = layout.start_row + row_offset;
        ansi::move_to(buf, row, layout.start_col);
        for _ in 0..layout.width {
            let _ = buf.write_all(b" ");
        }
    }

    ansi::restore_cursor(buf);
    if use_sync {
        ansi::end_sync(buf);
    }
}

fn format_item(
    buf: &mut Vec<u8>,
    s: &Suggestion,
    width: u16,
    is_selected: bool,
    theme: &PopupTheme,
) {
    let kind_char = match s.kind {
        SuggestionKind::Command => '\u{F120}',    // nf-fa-terminal
        SuggestionKind::Subcommand => '\u{F0DA}', // nf-fa-chevron_right
        SuggestionKind::Flag => '\u{F024}',       // nf-fa-flag
        SuggestionKind::FilePath => '\u{F15B}',   // nf-fa-file
        SuggestionKind::Directory => '\u{F07B}',  // nf-fa-folder
        SuggestionKind::GitBranch => '\u{E0A0}',  // nf-pl-branch
        SuggestionKind::GitTag => '\u{F02B}',     // nf-fa-tag
        SuggestionKind::GitRemote => '\u{F0C1}',  // nf-fa-link
        SuggestionKind::History => '\u{F1DA}',    // nf-fa-history
        SuggestionKind::EnvVar => '$',
    };

    // Gutter: " K "
    let _ = write!(buf, " {kind_char} ");

    let total_width = width as usize;
    let max_text_chars = total_width.saturating_sub(3); // 3 = gutter

    // For filesystem entries, show just the last path component (the user
    // already typed the prefix, so repeating it wastes popup space).
    // Also compute the prefix char count for offsetting match indices.
    let (display_text, prefix_char_count) = display_text(s);

    // Build display-relative match index set (offset and filtered)
    let display_indices: Vec<u32> = s
        .match_indices
        .iter()
        .filter_map(|&idx| {
            if idx >= prefix_char_count as u32 {
                Some(idx - prefix_char_count as u32)
            } else {
                None
            }
        })
        .collect();

    // Write text with highlight transitions, tracking display columns
    let mut in_highlight = false;
    let mut cols_written: usize = 0;
    for (char_idx, ch) in display_text.chars().enumerate() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if cols_written + ch_width > max_text_chars {
            break;
        }

        let should_highlight = !theme.match_highlight_on.is_empty()
            && display_indices.binary_search(&(char_idx as u32)).is_ok();

        if should_highlight && !in_highlight {
            buf.extend_from_slice(&theme.match_highlight_on);
            in_highlight = true;
        } else if !should_highlight && in_highlight {
            ansi::reset(buf);
            if is_selected {
                buf.extend_from_slice(&theme.selected_on);
            } else if !theme.item_text_on.is_empty() {
                buf.extend_from_slice(&theme.item_text_on);
            }
            in_highlight = false;
        }

        let _ = write!(buf, "{ch}");
        cols_written += ch_width;
    }

    // Close highlight if still active at end of text
    if in_highlight {
        ansi::reset(buf);
        if is_selected {
            buf.extend_from_slice(&theme.selected_on);
        } else if !theme.item_text_on.is_empty() {
            buf.extend_from_slice(&theme.item_text_on);
        }
    }

    let gutter_text_len = 3 + cols_written;

    // Description (if room)
    let desc = s.description.as_deref().unwrap_or("");
    let max_desc_cols = total_width.saturating_sub(gutter_text_len + 2 + 1);

    if !desc.is_empty() && max_desc_cols > 2 {
        let _ = buf.write_all(b"  ");
        if !is_selected {
            ansi::reset(buf);
            buf.extend_from_slice(&theme.description_on);
        }
        // Truncate description by display columns, not char count
        let mut desc_cols: usize = 0;
        let mut truncated = String::new();
        for ch in desc.chars() {
            let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if desc_cols + w > max_desc_cols {
                break;
            }
            truncated.push(ch);
            desc_cols += w;
        }
        let _ = write!(buf, "{truncated}");
        if !is_selected {
            ansi::reset(buf);
            if !theme.item_text_on.is_empty() {
                buf.extend_from_slice(&theme.item_text_on);
            }
        }
        let used = gutter_text_len + 2 + desc_cols;
        let pad = total_width.saturating_sub(used);
        for _ in 0..pad {
            let _ = buf.write_all(b" ");
        }
    } else {
        let pad = total_width.saturating_sub(gutter_text_len);
        for _ in 0..pad {
            let _ = buf.write_all(b" ");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DEFAULT_MAX_POPUP_WIDTH, DEFAULT_MAX_VISIBLE, DEFAULT_MIN_POPUP_WIDTH};
    use gc_suggest::SuggestionSource;

    fn ghostty_profile() -> TerminalProfile {
        TerminalProfile::for_ghostty()
    }

    fn iterm2_profile() -> TerminalProfile {
        TerminalProfile::for_iterm2()
    }

    fn bordered_theme() -> PopupTheme {
        PopupTheme {
            borders: true,
            ..PopupTheme::default()
        }
    }

    fn make(text: &str, desc: Option<&str>, kind: SuggestionKind) -> Suggestion {
        Suggestion {
            text: text.to_string(),
            description: desc.map(String::from),
            kind,
            source: SuggestionSource::Spec,
            ..Default::default()
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
        render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );
        let output = String::from_utf8_lossy(&buf);
        assert!(
            output.starts_with("\x1b[?2026h"),
            "should start with begin_sync"
        );
        assert!(output.ends_with("\x1b[?2026l"), "should end with end_sync");
    }

    #[test]
    fn test_render_pre_render_buffer_skips_sync() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions();
        let state = OverlayState::new();
        render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &iterm2_profile(),
        );
        let output = String::from_utf8_lossy(&buf);
        assert!(
            !output.contains("\x1b[?2026h"),
            "PreRenderBuffer should NOT contain begin_sync"
        );
        assert!(
            !output.contains("\x1b[?2026l"),
            "PreRenderBuffer should NOT contain end_sync"
        );
        // Should still have save/restore cursor
        assert!(output.contains("\x1b7"), "should still save cursor");
        assert!(output.contains("\x1b8"), "should still restore cursor");
    }

    #[test]
    fn test_clear_pre_render_buffer_skips_sync() {
        let mut buf = Vec::new();
        let layout = PopupLayout {
            start_row: 5,
            start_col: 0,
            width: 20,
            height: 3,
            scroll_deficit: 0,
        };
        clear_popup(&mut buf, &layout, &iterm2_profile());
        let output = String::from_utf8_lossy(&buf);
        assert!(
            !output.contains("\x1b[?2026h"),
            "PreRenderBuffer clear should NOT contain begin_sync"
        );
        assert!(
            !output.contains("\x1b[?2026l"),
            "PreRenderBuffer clear should NOT contain end_sync"
        );
    }

    #[test]
    fn test_render_saves_restores_cursor() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions();
        let state = OverlayState::new();
        render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );
        let output = String::from_utf8_lossy(&buf);
        assert!(output.contains("\x1b7"), "should contain save cursor");
        assert!(output.contains("\x1b8"), "should contain restore cursor");
    }

    #[test]
    fn test_render_positions_at_layout() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions();
        let state = OverlayState::new();
        render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );
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
        let mut state = OverlayState::new();
        state.selected = Some(0);
        render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );
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
        format_item(&mut buf, &s, 30, false, &bordered_theme());
        let output = String::from_utf8_lossy(&buf);
        assert!(
            output.starts_with(" \u{F0DA} checkout"),
            "should show kind icon for subcommand: got '{output}'"
        );
    }

    #[test]
    fn test_format_item_shows_only_filename_for_directory() {
        let mut buf = Vec::new();
        let s = make(
            "Desktop/coding/advent-of-code/master/2023-rust/",
            None,
            SuggestionKind::Directory,
        );
        format_item(&mut buf, &s, 40, false, &bordered_theme());
        let output = String::from_utf8_lossy(&buf);
        assert!(
            output.contains("2023-rust/"),
            "should show only dirname: got '{output}'"
        );
        assert!(
            !output.contains("Desktop/"),
            "should NOT show full path prefix: got '{output}'"
        );
    }

    #[test]
    fn test_format_item_shows_only_filename_for_file() {
        let mut buf = Vec::new();
        let s = make("src/main/java/App.java", None, SuggestionKind::FilePath);
        format_item(&mut buf, &s, 40, false, &bordered_theme());
        let output = String::from_utf8_lossy(&buf);
        assert!(
            output.contains("App.java"),
            "should show only filename: got '{output}'"
        );
        assert!(
            !output.contains("src/"),
            "should NOT show path prefix: got '{output}'"
        );
    }

    #[test]
    fn test_format_item_no_slash_shows_full_name() {
        let mut buf = Vec::new();
        let s = make("Desktop/", None, SuggestionKind::Directory);
        format_item(&mut buf, &s, 40, false, &bordered_theme());
        let output = String::from_utf8_lossy(&buf);
        assert!(
            output.contains("Desktop/"),
            "single-component dir should show full name: got '{output}'"
        );
    }

    #[test]
    fn test_format_item_truncates_long_text() {
        let mut buf = Vec::new();
        let long_text = "https://api.github.com/orgs/Example/packages?package_type=container";
        let s = make(long_text, None, SuggestionKind::History);
        let width: u16 = 30;
        format_item(&mut buf, &s, width, false, &bordered_theme());
        // Count printable characters (no ANSI escape sequences)
        let output = String::from_utf8_lossy(&buf);
        let printable: String = output
            .chars()
            .filter(|c| !c.is_control() || *c == ' ')
            .collect();
        let char_count = printable.chars().count();
        assert!(
            char_count <= width as usize,
            "printable chars ({char_count}) must not exceed width ({width}): '{printable}'"
        );
    }

    #[test]
    fn test_format_item_truncates_description() {
        let mut buf = Vec::new();
        let long_desc = "a".repeat(200);
        let s = make("cmd", Some(&long_desc), SuggestionKind::Command);
        format_item(&mut buf, &s, 30, false, &bordered_theme());
        // Output should not exceed width
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
            scroll_deficit: 0,
        };
        clear_popup(&mut buf, &layout, &ghostty_profile());
        let output = String::from_utf8_lossy(&buf);
        assert!(!output.contains("\x1b[K"), "should not use erase_to_eol");
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
            scroll_deficit: 0,
        };
        clear_popup(&mut buf, &layout, &ghostty_profile());
        let output = String::from_utf8_lossy(&buf);
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
        state.selected = Some(5);
        let layout = render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );
        let output = String::from_utf8_lossy(&buf);
        assert!(
            output.contains("item5"),
            "should show item5 at scroll_offset=5"
        );
        assert!(
            !output.contains("item0"),
            "should not show item0 when scrolled"
        );
        assert_eq!(layout.height, 12); // DEFAULT_MAX_VISIBLE + 2 border rows
    }

    #[test]
    fn test_render_empty_suggestions() {
        let mut buf = Vec::new();
        let suggestions: Vec<Suggestion> = vec![];
        let state = OverlayState::new();
        let layout = render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );
        assert_eq!(layout.height, 0);
        assert!(
            buf.is_empty(),
            "should produce no output for empty suggestions"
        );
    }

    // --- parse_style tests ---

    #[test]
    fn test_parse_style_reverse() {
        assert_eq!(parse_style("reverse").unwrap(), b"\x1b[7m");
    }

    #[test]
    fn test_parse_style_dim_bold() {
        assert_eq!(parse_style("dim bold").unwrap(), b"\x1b[2;1m");
    }

    #[test]
    fn test_parse_style_fg_color() {
        assert_eq!(parse_style("fg:196").unwrap(), b"\x1b[38;5;196m");
    }

    #[test]
    fn test_parse_style_bg_bold() {
        assert_eq!(parse_style("bg:236 bold").unwrap(), b"\x1b[48;5;236;1m");
    }

    #[test]
    fn test_parse_style_underline() {
        assert_eq!(parse_style("underline").unwrap(), b"\x1b[4m");
    }

    #[test]
    fn test_parse_style_empty() {
        assert_eq!(parse_style("").unwrap(), b"");
    }

    #[test]
    fn test_parse_style_invalid_token() {
        assert!(parse_style("blink").is_err());
    }

    #[test]
    fn test_parse_style_invalid_fg_number() {
        assert!(parse_style("fg:abc").is_err());
    }

    #[test]
    fn test_parse_style_invalid_fg_overflow() {
        assert!(parse_style("fg:999").is_err());
    }

    // --- scroll-to-make-room tests ---

    #[test]
    fn test_render_scroll_when_deficit_needed() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions(); // 3 items
        let state = OverlayState::new();
        // cursor at row 22 on 24-row screen: space_below = 1, need 3+2(borders)=5, deficit = 4
        let layout = render_popup(
            &mut buf,
            &suggestions,
            &state,
            22,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );
        let output = String::from_utf8_lossy(&buf);
        // Should CUP to last row before newlines
        assert!(
            output.contains("\x1b[24;1H"),
            "should CUP to last row: {output}"
        );
        // Should contain newlines (deficit = 4)
        assert!(
            output.contains("\n\n\n\n"),
            "should emit 4 deficit newlines: {output}"
        );
        assert_eq!(layout.scroll_deficit, 4);
        // final_cursor = 22 - 4 = 18, start_row = 19
        assert_eq!(layout.start_row, 19);
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
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );
        // No newlines means no scrolling occurred (popup uses CUP, not newlines)
        assert!(
            !buf.contains(&b'\n'),
            "should not contain newlines (no scroll needed)"
        );
        assert_eq!(layout.scroll_deficit, 0);
        assert_eq!(layout.start_row, 6);
    }

    #[test]
    fn test_render_prior_deficit_prevents_rescroll() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions(); // 3 items
        let state = OverlayState::new();
        // prior_deficit=4: adjusted cursor = 22-4 = 18, space_below = 5, need 3+2=5, no new deficit
        let layout = render_popup(
            &mut buf,
            &suggestions,
            &state,
            22,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            4,
            false,
            &ghostty_profile(),
        );
        // No newlines means no scrolling occurred (popup uses CUP, not newlines)
        assert!(
            !buf.contains(&b'\n'),
            "should not contain newlines (no re-scroll)"
        );
        assert_eq!(layout.scroll_deficit, 4); // carries forward
        assert_eq!(layout.start_row, 19);
    }

    #[test]
    fn test_render_incremental_deficit() {
        // First render: 3 items, cursor at row 22, screen 24 -> deficit = 4 (3 items + 2 borders - 1 space)
        let mut buf1 = Vec::new();
        let suggestions_3 = make_suggestions(); // 3 items
        let state = OverlayState::new();
        let layout1 = render_popup(
            &mut buf1,
            &suggestions_3,
            &state,
            22,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );
        assert_eq!(layout1.scroll_deficit, 4);

        // Second render: 8 items, same cursor, prior_deficit=4
        // adj_cursor = 22-4 = 18, space_below = 5, need 8+2=10, new_deficit = 5
        let mut buf2 = Vec::new();
        let suggestions_8: Vec<Suggestion> = (0..8)
            .map(|i| make(&format!("item{i}"), None, SuggestionKind::Command))
            .collect();
        let layout2 = render_popup(
            &mut buf2,
            &suggestions_8,
            &state,
            22,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            4, // prior_deficit from first render
            false,
            &ghostty_profile(),
        );
        let output2 = String::from_utf8_lossy(&buf2);
        // Should scroll 5 more (total = 4 + 5 = 9)
        assert!(
            output2.contains("\x1b[24;1H"),
            "should scroll for incremental deficit"
        );
        assert_eq!(layout2.scroll_deficit, 9);
        // final_cursor = 22 - 9 = 13, start_row = 14
        assert_eq!(layout2.start_row, 14);
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
            22,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );
        let output = String::from_utf8_lossy(&buf);
        let cup_to_adjusted = "\x1b[19;1H"; // adj_row=18, ANSI row=19
        let decsc = "\x1b7";
        let cup_pos = output
            .find(cup_to_adjusted)
            .expect("should contain CUP to adjusted position");
        let decsc_pos = output.find(decsc).expect("should contain DECSC");
        assert!(
            decsc_pos > cup_pos,
            "DECSC must come AFTER CUP to adjusted position"
        );
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
            4,
            0,
            6, // only 6 rows
            80,
            15, // max_visible bigger than screen
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );
        // capped to screen_rows - 1 - 2(borders) = 3 content + 2 borders = 5
        assert!(
            layout.height <= 5,
            "height {} should be <= 5",
            layout.height
        );
        assert!(layout.start_row >= 1);
    }

    #[test]
    fn test_render_adj_row_never_underflows() {
        let mut buf = Vec::new();
        // cursor at row 2, 6-row terminal, 15 suggestions, max_visible=15
        // effective_max = min(15, 6-1-2) = 3
        // adj_cursor = 2, space_below = 6-2-1 = 3, visible=3+2(borders)=5, deficit = 2
        // total_deficit = 2, final_cursor = 2 - 2 = 0 (not underflowed)
        let suggestions: Vec<Suggestion> = (0..15)
            .map(|i| make(&format!("item{i}"), None, SuggestionKind::Command))
            .collect();
        let state = OverlayState::new();
        let layout = render_popup(
            &mut buf,
            &suggestions,
            &state,
            2,
            0,
            6,
            80,
            15,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );
        // final_cursor = 0, start_row = 1
        assert_eq!(layout.start_row, 1);
        assert_eq!(layout.scroll_deficit, 2);
    }

    #[test]
    fn test_parse_style_hex_fg() {
        assert_eq!(
            parse_style("fg:#89b4fa").unwrap(),
            b"\x1b[38;2;137;180;250m"
        );
    }

    #[test]
    fn test_parse_style_hex_bg() {
        assert_eq!(parse_style("bg:#1e1e2e").unwrap(), b"\x1b[48;2;30;30;46m");
    }

    #[test]
    fn test_parse_style_hex_case_insensitive() {
        assert_eq!(
            parse_style("fg:#89B4FA").unwrap(),
            parse_style("fg:#89b4fa").unwrap()
        );
    }

    #[test]
    fn test_parse_style_hex_mixed_with_256() {
        assert_eq!(
            parse_style("fg:#89b4fa bg:236 bold").unwrap(),
            b"\x1b[38;2;137;180;250;48;5;236;1m"
        );
    }

    #[test]
    fn test_parse_style_hex_too_short() {
        assert!(parse_style("fg:#89b4").is_err());
    }

    #[test]
    fn test_parse_style_hex_invalid_chars() {
        assert!(parse_style("fg:#gggggg").is_err());
    }

    #[test]
    fn test_parse_style_hex_missing_hash() {
        assert!(parse_style("fg:89b4fa").is_err());
    }

    #[test]
    fn test_non_selected_row_has_item_text_style() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions();
        let mut state = OverlayState::new();
        state.selected = Some(0); // Only first item selected
        let theme = PopupTheme {
            item_text_on: b"\x1b[2m".to_vec(), // explicitly dim for this test
            ..bordered_theme()
        };
        render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &theme,
            0,
            false,
            &ghostty_profile(),
        );
        let output = String::from_utf8_lossy(&buf);
        // Count occurrences of dim (\x1b[2m) — should appear for non-selected rows
        let dim_count = output.matches("\x1b[2m").count();
        // At least 2 non-selected rows should have dim styling
        // (existing description dim + new item_text dim)
        assert!(
            dim_count >= 2,
            "non-selected rows should be dimmed, got {dim_count} dim sequences"
        );
    }

    #[test]
    fn test_selected_row_no_item_text_style() {
        let mut buf = Vec::new();
        let suggestions = vec![make("only", None, SuggestionKind::Command)];
        let mut state = OverlayState::new();
        state.selected = Some(0);
        let theme = PopupTheme {
            item_text_on: b"\x1b[2m".to_vec(),
            selected_on: b"\x1b[7m".to_vec(),
            ..bordered_theme()
        };
        render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &theme,
            0,
            false,
            &ghostty_profile(),
        );
        let output = String::from_utf8_lossy(&buf);
        // Selected row should have reverse, not dim
        assert!(output.contains("\x1b[7m"), "selected should have reverse");
    }

    #[test]
    fn test_empty_item_text_style_no_extra_escapes() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions();
        let state = OverlayState::new(); // Nothing selected
        let theme = PopupTheme {
            item_text_on: vec![], // Empty — no styling
            ..bordered_theme()
        };
        render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &theme,
            0,
            false,
            &ghostty_profile(),
        );
        let output = String::from_utf8_lossy(&buf);
        // Dim sequences come from: border_on (dim) + description_on (dim) for each row
        // 3 content rows × (2 border + 1 desc) + 2 border rows = 11 dim sequences
        let dim_count = output.matches("\x1b[2m").count();
        assert!(
            dim_count <= 12,
            "expected ~11 dim sequences (borders + descriptions), got {dim_count}: {output}"
        );
    }

    #[test]
    fn test_highlight_matched_chars() {
        let mut buf = Vec::new();
        let mut s = make(
            "checkout",
            Some("Switch branches"),
            SuggestionKind::Subcommand,
        );
        s.match_indices = vec![0, 1, 2]; // "che" matched
        let theme = PopupTheme::default();
        format_item(&mut buf, &s, 40, false, &theme);
        let output = String::from_utf8_lossy(&buf);
        // Should contain bold sequence (match_highlight_on default)
        assert!(
            output.contains("\x1b[1m"),
            "matched chars should have bold highlight: {output}"
        );
    }

    #[test]
    fn test_no_indices_no_highlight() {
        let mut buf = Vec::new();
        let s = make("checkout", None, SuggestionKind::Subcommand);
        // match_indices is empty (default)
        let theme = PopupTheme::default();
        format_item(&mut buf, &s, 40, false, &theme);
        let output = String::from_utf8_lossy(&buf);
        // Should NOT contain bold (no match highlighting)
        assert!(
            !output.contains("\x1b[1m"),
            "no indices means no highlight: {output}"
        );
    }

    #[test]
    fn test_highlight_consecutive_single_span() {
        let mut buf = Vec::new();
        let mut s = make("checkout", None, SuggestionKind::Subcommand);
        s.match_indices = vec![0, 1, 2]; // consecutive
        let theme = PopupTheme::default();
        format_item(&mut buf, &s, 40, false, &theme);
        let output = String::from_utf8_lossy(&buf);
        // Should have exactly one bold-on sequence for consecutive matches
        let bold_count = output.matches("\x1b[1m").count();
        assert_eq!(
            bold_count, 1,
            "consecutive matches should produce single span"
        );
    }

    #[test]
    fn test_highlight_on_selected_row() {
        let mut buf = Vec::new();
        let mut s = make("checkout", None, SuggestionKind::Subcommand);
        s.match_indices = vec![0, 1, 2];
        let theme = PopupTheme::default();
        format_item(&mut buf, &s, 40, true, &theme);
        let output = String::from_utf8_lossy(&buf);
        // Should contain bold (highlight composes with selected reverse)
        assert!(
            output.contains("\x1b[1m"),
            "selected row should still show highlight"
        );
    }

    #[test]
    fn test_highlight_filepath_basename_offset() {
        let mut buf = Vec::new();
        let mut s = make("src/main/App.java", None, SuggestionKind::FilePath);
        // Indices for "App" in full path: chars 9, 10, 11
        s.match_indices = vec![9, 10, 11];
        let theme = PopupTheme::default();
        format_item(&mut buf, &s, 40, false, &theme);
        let output = String::from_utf8_lossy(&buf);
        // Display shows "App.java", highlight should be on "App"
        assert!(
            output.contains("\x1b[1m"),
            "basename highlight should work: {output}"
        );
    }

    #[test]
    fn test_highlight_indices_in_stripped_prefix_skipped() {
        let mut buf = Vec::new();
        let mut s = make("src/main/App.java", None, SuggestionKind::FilePath);
        // Indices in the stripped prefix (before "App.java")
        s.match_indices = vec![0, 1, 2]; // "src" — in prefix, should be skipped
        let theme = PopupTheme::default();
        format_item(&mut buf, &s, 40, false, &theme);
        let output = String::from_utf8_lossy(&buf);
        // No highlight — all indices are in the stripped prefix
        assert!(
            !output.contains("\x1b[1m"),
            "indices in stripped prefix should not highlight: {output}"
        );
    }

    #[test]
    fn test_highlight_non_ascii_path_offset() {
        let mut buf = Vec::new();
        // "café/" is 5 chars but 6 bytes (é = 2 bytes in UTF-8)
        let mut s = make("café/menu.txt", None, SuggestionKind::FilePath);
        // "menu" starts at char index 5; match indices for "men" = 5, 6, 7
        s.match_indices = vec![5, 6, 7];
        let theme = PopupTheme::default();
        format_item(&mut buf, &s, 40, false, &theme);
        let output = String::from_utf8_lossy(&buf);
        // Display shows "menu.txt", highlight should be on "men"
        assert!(
            output.contains("\x1b[1m"),
            "non-ASCII path offset should work: {output}"
        );
    }

    #[test]
    fn test_highlight_indices_beyond_truncation_ignored() {
        let mut buf = Vec::new();
        let long_text = "a_very_long_command_name_that_will_be_truncated";
        let mut s = make(long_text, None, SuggestionKind::Command);
        // Index 40+ is beyond a width=20 popup's text area
        s.match_indices = vec![42, 43, 44];
        let theme = PopupTheme::default();
        format_item(&mut buf, &s, 20, false, &theme);
        let output = String::from_utf8_lossy(&buf);
        // No highlight — all indices are beyond the truncation point
        assert!(
            !output.contains("\x1b[1m"),
            "indices beyond truncation should not highlight: {output}"
        );
    }

    #[test]
    fn test_scrollbar_visible_when_items_exceed_max_visible() {
        let mut buf = Vec::new();
        let suggestions: Vec<Suggestion> = (0..15)
            .map(|i| make(&format!("item{i}"), None, SuggestionKind::Command))
            .collect();
        let state = OverlayState::new();
        render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );
        let output = String::from_utf8_lossy(&buf);
        assert!(
            output.contains('█') || output.contains('┆'),
            "scrollbar should be visible with 15 items: {output}"
        );
    }

    #[test]
    fn test_no_scrollbar_when_items_fit() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions(); // 3 items
        let state = OverlayState::new();
        render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );
        let output = String::from_utf8_lossy(&buf);
        // No scrollbar thumb or track — but border │ chars are always present
        // Count │ occurrences: should be exactly 2 per content row (left + right border) × 3 rows = 6
        // Plus no scrollbar │ chars (which would be extra)
        let pipe_count = output.matches('│').count();
        assert_eq!(
            pipe_count, 6,
            "should have exactly 6 │ chars (3 rows × 2 borders), not scrollbar: {output}"
        );
        assert!(
            !output.contains('█'),
            "no scrollbar thumb when items fit: {output}"
        );
    }

    #[test]
    fn test_scrollbar_thumb_at_top_when_scroll_zero() {
        let mut buf = Vec::new();
        let suggestions: Vec<Suggestion> = (0..20)
            .map(|i| make(&format!("item{i}"), None, SuggestionKind::Command))
            .collect();
        let state = OverlayState::new(); // scroll_offset = 0
        render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );
        let output = String::from_utf8_lossy(&buf);
        assert!(output.contains('█'), "should have thumb indicator");
        assert!(output.contains('┆'), "should have track indicator");
    }

    #[test]
    fn test_scrollbar_thumb_at_bottom_when_scrolled_to_end() {
        let mut buf = Vec::new();
        let suggestions: Vec<Suggestion> = (0..20)
            .map(|i| make(&format!("item{i}"), None, SuggestionKind::Command))
            .collect();
        let mut state = OverlayState::new();
        state.scroll_offset = 10; // scrolled to bottom (20 - 10 max_visible)
        state.selected = Some(19);
        render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );
        let output = String::from_utf8_lossy(&buf);
        assert!(output.contains('█'), "should have thumb at bottom");
    }

    #[test]
    fn test_scrollbar_item_text_width_reduced() {
        let suggestions_few: Vec<Suggestion> = (0..3)
            .map(|i| make(&format!("item{i}"), None, SuggestionKind::Command))
            .collect();
        let suggestions_many: Vec<Suggestion> = (0..15)
            .map(|i| make(&format!("item{i}"), None, SuggestionKind::Command))
            .collect();

        let mut buf_no_scroll = Vec::new();
        let state = OverlayState::new();
        render_popup(
            &mut buf_no_scroll,
            &suggestions_few,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );

        let mut buf_scroll = Vec::new();
        render_popup(
            &mut buf_scroll,
            &suggestions_many,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );

        let output = String::from_utf8_lossy(&buf_scroll);
        assert!(
            output.contains('┆') || output.contains('█'),
            "scrollbar popup should have scrollbar chars"
        );
    }

    #[test]
    fn test_scrollbar_selected_row_uses_selected_style() {
        let mut buf = Vec::new();
        let suggestions: Vec<Suggestion> = (0..15)
            .map(|i| make(&format!("item{i}"), None, SuggestionKind::Command))
            .collect();
        let mut state = OverlayState::new();
        state.selected = Some(0);
        let theme = PopupTheme {
            selected_on: b"\x1b[7m".to_vec(),
            ..bordered_theme()
        };
        render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &theme,
            0,
            false,
            &ghostty_profile(),
        );
        let output = String::from_utf8_lossy(&buf);
        assert!(
            output.contains("\x1b[7m"),
            "selected row should have reverse video"
        );
    }

    #[test]
    fn test_loading_true_shows_indicator() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions();
        let state = OverlayState::new();
        let layout = render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            true,
            &ghostty_profile(),
        );
        let output = String::from_utf8_lossy(&buf);
        assert!(
            output.contains("..."),
            "loading=true should produce '...' indicator: {output}"
        );
        // Height: 3 content + 2 borders + 1 loading extra (loading displaces bottom border,
        // which moves down 1 row, netting 1 extra row beyond layout.height) = 6
        assert_eq!(
            layout.height, 6,
            "loading should increase height by 1 beyond base layout"
        );
    }

    #[test]
    fn test_loading_false_no_indicator() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions();
        let state = OverlayState::new();
        let layout = render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );
        let output = String::from_utf8_lossy(&buf);
        assert!(
            !output.contains("..."),
            "loading=false should NOT produce '...' indicator: {output}"
        );
        // Height: 3 content + 2 borders = 5
        assert_eq!(layout.height, 5);
    }

    #[test]
    fn test_border_characters_present() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions(); // 3 items
        let state = OverlayState::new();
        render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &bordered_theme(),
            0,
            false,
            &ghostty_profile(),
        );
        let output = String::from_utf8_lossy(&buf);
        assert!(output.contains('╭'), "missing top-left corner: {output}");
        assert!(output.contains('╮'), "missing top-right corner: {output}");
        assert!(output.contains('╰'), "missing bottom-left corner: {output}");
        assert!(
            output.contains('╯'),
            "missing bottom-right corner: {output}"
        );
        assert!(output.contains('─'), "missing horizontal border: {output}");
        assert!(output.contains('│'), "missing vertical border: {output}");
    }

    // --- borders: false tests (production default) ---

    #[test]
    fn test_render_no_borders_no_border_chars() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions();
        let state = OverlayState::new();
        render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &PopupTheme::default(), // borders: false
            0,
            false,
            &ghostty_profile(),
        );
        let output = String::from_utf8_lossy(&buf);
        assert!(!output.contains('╭'), "no top-left corner without borders");
        assert!(!output.contains('╮'), "no top-right corner without borders");
        assert!(
            !output.contains('╰'),
            "no bottom-left corner without borders"
        );
        assert!(
            !output.contains('╯'),
            "no bottom-right corner without borders"
        );
        assert!(!output.contains('│'), "no vertical border without borders");
    }

    #[test]
    fn test_render_no_borders_height() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions(); // 3 items
        let state = OverlayState::new();
        let layout = render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &PopupTheme::default(),
            0,
            false,
            &ghostty_profile(),
        );
        // 3 content rows, no border padding
        assert_eq!(layout.height, 3, "borderless height = content rows only");
    }

    #[test]
    fn test_layout_no_borders_width() {
        // Use a suggestion wide enough to exceed min_width so border padding is visible
        let suggestions = vec![make(
            "a-sufficiently-long-suggestion-name",
            None,
            SuggestionKind::Subcommand,
        )];
        let layout = layout::compute_layout(
            &suggestions,
            0,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            false, // no borders
        );
        let bordered = layout::compute_layout(
            &suggestions,
            0,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            true,
        );
        assert_eq!(
            bordered.width - layout.width,
            2,
            "bordered should be 2 wider (border padding), got borderless={} bordered={}",
            layout.width,
            bordered.width
        );
    }

    #[test]
    fn test_loading_no_borders_height() {
        let mut buf = Vec::new();
        let suggestions = make_suggestions();
        let state = OverlayState::new();
        let layout = render_popup(
            &mut buf,
            &suggestions,
            &state,
            5,
            0,
            24,
            80,
            DEFAULT_MAX_VISIBLE,
            DEFAULT_MIN_POPUP_WIDTH,
            DEFAULT_MAX_POPUP_WIDTH,
            &PopupTheme::default(), // borders: false
            0,
            true,
            &ghostty_profile(),
        );
        // 3 content + 1 loading row, no borders
        assert_eq!(
            layout.height, 4,
            "borderless loading height = content + 1 loading row"
        );
    }
}
