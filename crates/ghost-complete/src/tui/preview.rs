use gc_overlay::frame::{build_popup_frame, PopupRow, ScrollbarCell, SpanStyle};
use gc_overlay::types::{OverlayState, PopupLayout};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::app::App;
use super::sample::sample_suggestions;

/// Parse a Ghost Complete style string (e.g. "reverse bold fg:#FF0000") into a
/// ratatui `Style`. Same token vocabulary as `gc_overlay::parse_style` but
/// outputting ratatui types instead of ANSI bytes.
pub fn parse_ratatui_style(style_str: &str) -> Style {
    let mut style = Style::default();
    for token in style_str.split_whitespace() {
        match token {
            "reverse" => style = style.add_modifier(Modifier::REVERSED),
            "dim" => style = style.add_modifier(Modifier::DIM),
            "bold" => style = style.add_modifier(Modifier::BOLD),
            "underline" => style = style.add_modifier(Modifier::UNDERLINED),
            _ if token.starts_with("fg:#") => {
                if let Some(color) = parse_hex_color(&token[4..]) {
                    style = style.fg(color);
                }
            }
            _ if token.starts_with("bg:#") => {
                if let Some(color) = parse_hex_color(&token[4..]) {
                    style = style.bg(color);
                }
            }
            _ if token.starts_with("fg:") => {
                if let Ok(n) = token[3..].parse::<u8>() {
                    style = style.fg(Color::Indexed(n));
                }
            }
            _ if token.starts_with("bg:") => {
                if let Ok(n) = token[3..].parse::<u8>() {
                    style = style.bg(Color::Indexed(n));
                }
            }
            _ => {}
        }
    }
    style
}

fn parse_hex_color(hex: &str) -> Option<Color> {
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

/// Render the popup preview pane.
pub fn render_preview(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Preview ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Bail if too small
    if inner.width < 10 || inner.height < 3 {
        return;
    }

    // Resolve theme styles
    let resolved = app.config.theme.resolve().unwrap_or_default();
    let selected_style = parse_ratatui_style(&resolved.selected);
    let description_style = parse_ratatui_style(&resolved.description);
    let match_highlight_style = parse_ratatui_style(&resolved.match_highlight);
    let item_text_style = parse_ratatui_style(&resolved.item_text);
    let scrollbar_style = parse_ratatui_style(&resolved.scrollbar);
    let border_style = parse_ratatui_style(&resolved.border);

    // Build popup frame from sample suggestions
    let suggestions = sample_suggestions();
    let mut state = OverlayState::new();
    state.selected = Some(0); // first item selected

    let max_visible = app.config.popup.max_visible;
    let borders = app.config.popup.borders;

    let height = if borders {
        inner.height.min((max_visible as u16).saturating_add(2))
    } else {
        inner.height.min(max_visible as u16)
    };

    // Clamp popup width to leave padding inside the preview pane so
    // popup borders don't overlap the preview Block borders.
    let popup_width = inner.width.saturating_sub(2).min(60);

    let layout = PopupLayout {
        start_row: 0,
        start_col: 0,
        width: popup_width,
        height,
        scroll_deficit: 0,
    };

    let Some(popup_frame) =
        build_popup_frame(&suggestions, &state, &layout, max_visible, borders, false)
    else {
        return;
    };

    // Map PopupFrame rows to ratatui Lines
    let lines: Vec<Line> = popup_frame
        .rows
        .iter()
        .map(|row| match row {
            PopupRow::Border { text } => Line::from(Span::styled(text.clone(), border_style)),
            PopupRow::Content(content_row) => {
                let base_style = if content_row.is_selected {
                    selected_style
                } else {
                    item_text_style
                };

                let mut spans: Vec<Span> = content_row
                    .spans
                    .iter()
                    .map(|styled_span| {
                        let style = if content_row.is_selected {
                            match styled_span.style {
                                SpanStyle::MatchHighlight => {
                                    selected_style.add_modifier(Modifier::BOLD)
                                }
                                _ => selected_style,
                            }
                        } else {
                            match styled_span.style {
                                SpanStyle::Plain | SpanStyle::Gutter => base_style,
                                SpanStyle::MatchHighlight => match_highlight_style,
                                SpanStyle::Description => description_style,
                                SpanStyle::Scrollbar => scrollbar_style,
                                SpanStyle::Border => border_style,
                                SpanStyle::Loading => description_style,
                            }
                        };
                        Span::styled(styled_span.text.clone(), style)
                    })
                    .collect();

                // Append scrollbar character
                match content_row.scrollbar {
                    ScrollbarCell::Thumb => {
                        spans.push(Span::styled("\u{2588}", scrollbar_style));
                    }
                    ScrollbarCell::Track => {
                        spans.push(Span::styled("\u{2502}", scrollbar_style));
                    }
                    ScrollbarCell::None => {}
                }

                Line::from(spans)
            }
            PopupRow::Loading { spans } => {
                let ratatui_spans: Vec<Span> = spans
                    .iter()
                    .map(|s| Span::styled(s.text.clone(), description_style))
                    .collect();
                Line::from(ratatui_spans)
            }
        })
        .collect();

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_style() {
        let s = parse_ratatui_style("");
        assert_eq!(s, Style::default());
    }

    #[test]
    fn parse_reverse() {
        let s = parse_ratatui_style("reverse");
        assert!(s.add_modifier == Modifier::REVERSED);
    }

    #[test]
    fn parse_bold_dim() {
        let s = parse_ratatui_style("bold dim");
        assert!(s.add_modifier.contains(Modifier::BOLD));
        assert!(s.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn parse_hex_fg() {
        let s = parse_ratatui_style("fg:#ff0000");
        assert_eq!(s.fg, Some(Color::Rgb(255, 0, 0)));
    }

    #[test]
    fn parse_hex_bg() {
        let s = parse_ratatui_style("bg:#00ff00");
        assert_eq!(s.bg, Some(Color::Rgb(0, 255, 0)));
    }

    #[test]
    fn parse_indexed_fg() {
        let s = parse_ratatui_style("fg:196");
        assert_eq!(s.fg, Some(Color::Indexed(196)));
    }

    #[test]
    fn parse_indexed_bg() {
        let s = parse_ratatui_style("bg:42");
        assert_eq!(s.bg, Some(Color::Indexed(42)));
    }

    #[test]
    fn parse_compound_style() {
        let s = parse_ratatui_style("bold fg:#1e1e2e bg:#dce0e8");
        assert!(s.add_modifier.contains(Modifier::BOLD));
        assert_eq!(s.fg, Some(Color::Rgb(0x1e, 0x1e, 0x2e)));
        assert_eq!(s.bg, Some(Color::Rgb(0xdc, 0xe0, 0xe8)));
    }
}
