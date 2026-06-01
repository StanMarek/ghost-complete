use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, EditState, Focus, Prompt};
use super::fields::{self, FieldMeta, ReloadBehavior, SECTIONS};
use super::preview;

/// Layout the editor adopts for the current frame. Selected purely from
/// `area.width`; key handlers consult [`layout_for_width`] when they need
/// to interpret scroll/preview semantics consistently with what was drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    /// >=100 cols: sections | fields | preview, all visible.
    ThreePane,
    /// 60-99 cols: sections | fields, preview only when `show_preview`.
    TwoPane,
    /// <60 cols: section selector on a single row above fields; no preview.
    Compact,
}

pub fn layout_for_width(width: u16) -> LayoutKind {
    if width >= 100 {
        LayoutKind::ThreePane
    } else if width >= 60 {
        LayoutKind::TwoPane
    } else {
        LayoutKind::Compact
    }
}

/// Top-level render function. Draws an adaptive layout + footer.
pub fn render(frame: &mut Frame, app: &App) {
    let size = frame.area();

    // Vertical split: main area + footer
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(2)])
        .split(size);

    let main_area = vertical[0];
    let footer_area = vertical[1];

    match layout_for_width(main_area.width) {
        LayoutKind::ThreePane => {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(16),
                    Constraint::Fill(1),
                    Constraint::Length(50),
                ])
                .split(main_area);
            render_sections(frame, app, chunks[0]);
            render_fields(frame, app, chunks[1]);
            preview::render_preview(frame, app, chunks[2]);
        }
        LayoutKind::TwoPane => {
            if app.show_preview {
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Length(14),
                        Constraint::Fill(1),
                        Constraint::Length(40),
                    ])
                    .split(main_area);
                render_sections(frame, app, chunks[0]);
                render_fields(frame, app, chunks[1]);
                preview::render_preview(frame, app, chunks[2]);
            } else {
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Length(14), Constraint::Fill(1)])
                    .split(main_area);
                render_sections(frame, app, chunks[0]);
                render_fields(frame, app, chunks[1]);
            }
        }
        LayoutKind::Compact => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Fill(1)])
                .split(main_area);
            render_sections_inline(frame, app, chunks[0]);
            render_fields(frame, app, chunks[1]);
        }
    }

    render_footer(frame, app, footer_area);

    if let Some(prompt) = app.prompt {
        render_prompt(frame, size, prompt, app);
    }
}

fn render_prompt(frame: &mut Frame, area: Rect, prompt: Prompt, app: &App) {
    // FilterInput is rendered inline in the footer rather than as a centered
    // modal — typing a query into a popup that covers the field list defeats
    // the purpose of filtering.
    if prompt == Prompt::FilterInput {
        return;
    }

    let (title, lines) = match prompt {
        Prompt::ConfirmQuit => (
            " Unsaved Changes ",
            vec![
                Line::from("You have unsaved edits."),
                Line::from(""),
                Line::from(vec![
                    Span::styled("y", Style::default().fg(Color::Cyan)),
                    Span::raw(" discard & quit   "),
                    Span::styled("n", Style::default().fg(Color::Cyan)),
                    Span::raw(" cancel   "),
                    Span::styled("s", Style::default().fg(Color::Cyan)),
                    Span::raw(" save & quit"),
                ]),
            ],
        ),
        Prompt::FileChangedOnDisk => (
            " File Changed On Disk ",
            vec![
                Line::from("The config was modified externally since load."),
                Line::from("Saving now would overwrite those changes."),
                Line::from(""),
                Line::from(vec![
                    Span::styled("r", Style::default().fg(Color::Cyan)),
                    Span::raw(" reload   "),
                    Span::styled("o", Style::default().fg(Color::Cyan)),
                    Span::raw(" overwrite   "),
                    Span::styled("c", Style::default().fg(Color::Cyan)),
                    Span::raw(" cancel"),
                ]),
            ],
        ),
        Prompt::FilterInput => unreachable!("handled above"),
    };
    // Silence unused-warning when no centered modal is rendered (FilterInput
    // path returns early).
    let _ = app;

    let width = 60.min(area.width.saturating_sub(4));
    let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(4));
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let rect = Rect {
        x,
        y,
        width,
        height,
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let paragraph = Paragraph::new(lines).block(block);

    // `Clear` wipes whatever the main layout drew underneath so the modal reads
    // clearly even on top of dense field rows.
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

fn render_sections(frame: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = SECTIONS
        .iter()
        .map(|s| {
            let label = fields::section_label(s);
            ListItem::new(Line::from(format!("  {label}")))
        })
        .collect();

    let block = Block::default()
        .title(" Sections ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let highlight_style = if app.focus == Focus::Sections {
        Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Cyan)
    };

    let list = List::new(items)
        .block(block)
        .highlight_style(highlight_style);

    let mut state = ListState::default().with_selected(Some(app.section_idx));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_fields(frame: &mut Frame, app: &App, area: Rect) {
    let section = app.current_section();
    let displayed = app.displayed_fields();

    let title = if app.filter.is_some() {
        format!(" Filtered ({} matches) ", displayed.len())
    } else {
        format!(" {} ", fields::section_label(section))
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if displayed.is_empty() {
        let msg = if app.filter.is_some() {
            "No fields match the current filter."
        } else {
            "No fields in this section."
        };
        let para = Paragraph::new(Line::from(Span::styled(
            format!("  {msg}"),
            Style::default().fg(Color::DarkGray),
        )));
        frame.render_widget(para, inner);
        return;
    }

    let mut lines: Vec<Line> = Vec::new();

    for (i, field) in displayed.iter().enumerate() {
        let is_selected = app.focus == Focus::Fields && i == app.field_idx;
        let value = app.field_value(field);

        // When filtering across sections, prefix the field name with its
        // section so the user can tell `theme.preset` from `popup.preset` at
        // a glance. In the unfiltered path the section pane already conveys
        // this, so we keep the bare key.
        let name_prefix = if app.filter.is_some() {
            format!("  {}.{}", field.section, field.key)
        } else {
            format!("  {}", field.key)
        };

        let name_style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Yellow)
        };

        let mut name_spans: Vec<Span> = vec![Span::styled(name_prefix, name_style)];

        if field.reload == ReloadBehavior::RequiresRestart {
            name_spans.push(Span::styled(
                " [restart]",
                Style::default().fg(Color::DarkGray),
            ));
        }

        lines.push(Line::from(name_spans));

        // Value line
        let value_line = if is_selected {
            if let EditState::Text { ref buffer, cursor } = app.edit_state {
                // In edit mode: show editable buffer with cursor
                let byte_cursor = buffer
                    .char_indices()
                    .nth(cursor)
                    .map(|(b, _)| b)
                    .unwrap_or(buffer.len());
                let before = &buffer[..byte_cursor];
                let after = &buffer[byte_cursor..];
                Line::from(vec![
                    Span::styled("  > ", Style::default().fg(Color::Green)),
                    Span::raw(before),
                    Span::styled("_", Style::default().add_modifier(Modifier::REVERSED)),
                    Span::raw(after),
                ])
            } else {
                Line::from(vec![
                    Span::styled("  = ", Style::default().fg(Color::DarkGray)),
                    Span::styled(value, Style::default().fg(Color::White)),
                ])
            }
        } else {
            Line::from(vec![
                Span::styled("  = ", Style::default().fg(Color::DarkGray)),
                Span::raw(value),
            ])
        };
        lines.push(value_line);

        // Help text
        lines.push(Line::from(Span::styled(
            format!("    {}", field.help),
            Style::default().fg(Color::DarkGray),
        )));

        // Blank separator
        lines.push(Line::from(""));
    }

    // Show validation errors at bottom
    let field_errors = visible_errors(app, &displayed);

    for (key, msg) in &field_errors {
        lines.push(Line::from(Span::styled(
            format!("  Error ({key}): {msg}"),
            Style::default().fg(Color::Red),
        )));
    }

    // Each field block is four rendered rows tall (name + value + help + blank).
    // The total line budget cannot exceed `u16::MAX` because ratatui's
    // `Paragraph::scroll` takes a `u16` row offset and we have to clamp.
    let visible_rows = inner.height;
    let max_scroll = app.max_field_scroll_for(visible_rows);
    let scroll = app.field_scroll.min(max_scroll);
    let paragraph = Paragraph::new(lines)
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

/// Compact-mode section selector — a single horizontal row instead of a side
/// pane, so the field list gets the remaining height when the terminal is
/// narrower than 60 columns.
fn render_sections_inline(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = Vec::with_capacity(SECTIONS.len() * 2);
    for (idx, section) in SECTIONS.iter().enumerate() {
        let label = fields::section_label(section);
        let style = if idx == app.section_idx && app.focus == Focus::Sections {
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD)
        } else if idx == app.section_idx {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(format!(" {label} "), style));
        if idx + 1 < SECTIONS.len() {
            spans.push(Span::styled("·", Style::default().fg(Color::DarkGray)));
        }
    }

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let para = Paragraph::new(Line::from(spans)).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    // FilterInput overrides the keybinding hint line — the user is typing into
    // the filter buffer and needs to see what they've typed plus an "N / M
    // matched" count, not the standard nav cheatsheet.
    if app.in_filter_input() {
        let query = app.filter.as_deref().unwrap_or("");
        let matched = app.displayed_fields().len();
        let total = app.all_fields.len();
        let line1 = Line::from(vec![
            Span::styled(" / ", Style::default().fg(Color::Green)),
            Span::raw(query),
            Span::styled("_", Style::default().add_modifier(Modifier::REVERSED)),
        ]);
        let line2 = Line::from(vec![
            Span::styled(
                format!(" Matched {matched} / {total} fields"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled("Enter", Style::default().fg(Color::Cyan)),
            Span::raw(" commit  "),
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::raw(" cancel"),
        ]);
        let para = Paragraph::new(vec![line1, line2]);
        frame.render_widget(para, area);
        return;
    }

    let mode_str = match app.edit_state {
        EditState::None => "Navigate",
        EditState::Text { .. } => "Edit",
    };

    let mut spans: Vec<Span> = vec![
        Span::styled(" Tab", Style::default().fg(Color::Cyan)),
        Span::raw(" switch  "),
        Span::styled("Enter", Style::default().fg(Color::Cyan)),
        Span::raw(" edit  "),
        Span::styled("/", Style::default().fg(Color::Cyan)),
        Span::raw(" filter  "),
        Span::styled("Ctrl+S", Style::default().fg(Color::Cyan)),
        Span::raw(" save  "),
        Span::styled("q/Esc", Style::default().fg(Color::Cyan)),
        Span::raw(" quit"),
    ];

    if app.dirty {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "[modified]",
            Style::default().fg(Color::Yellow),
        ));
    }

    let footer_line_1 = Line::from(spans);
    let mut footer_line_2_spans = vec![
        Span::styled(
            format!(" Mode: {mode_str}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw("  "),
    ];

    if let Some(query) = &app.filter {
        let matched = app.displayed_fields().len();
        let total = app.all_fields.len();
        footer_line_2_spans.push(Span::styled(
            format!("Filter: /{query}  ({matched} / {total})"),
            Style::default().fg(Color::Green),
        ));
    } else {
        footer_line_2_spans.push(Span::styled(
            format!("Section: {}", fields::section_label(app.current_section())),
            Style::default().fg(Color::DarkGray),
        ));
    }

    if let Some(status) = footer_status(app) {
        footer_line_2_spans.push(Span::raw("  "));
        footer_line_2_spans.push(Span::styled(status, Style::default().fg(Color::Red)));
    }

    let footer_line_2 = Line::from(footer_line_2_spans);

    let paragraph = Paragraph::new(vec![footer_line_1, footer_line_2]);
    frame.render_widget(paragraph, area);
}

fn footer_status(app: &App) -> Option<String> {
    app.errors
        .iter()
        .find(|(key, _)| key == "save")
        .map(|(_, msg)| msg.clone())
}

fn visible_errors<'a>(app: &'a App, section_fields: &[&FieldMeta]) -> Vec<&'a (String, String)> {
    app.errors
        .iter()
        .filter(|(key, _)| {
            section_fields
                .iter()
                .any(|f| format!("{}.{}", f.section, f.key) == *key)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gc_config::GhostConfig;
    use std::path::PathBuf;

    #[test]
    fn visible_errors_exclude_global_save_errors() {
        let mut app = App::new(
            GhostConfig::default(),
            String::new(),
            PathBuf::from("/tmp/config.toml"),
        );
        app.errors
            .push(("save".to_string(), "write failed".to_string()));

        let section_fields = app.current_section_fields();
        let visible = visible_errors(&app, &section_fields);

        assert!(!visible
            .iter()
            .any(|(key, msg)| key == "save" && msg == "write failed"));
    }

    #[test]
    fn footer_status_prefers_save_error() {
        let mut app = App::new(
            GhostConfig::default(),
            String::new(),
            PathBuf::from("/tmp/config.toml"),
        );
        app.errors
            .push(("save".to_string(), "write failed".to_string()));

        assert_eq!(footer_status(&app), Some("write failed".to_string()));
    }

    #[test]
    fn layout_for_width_thresholds() {
        assert_eq!(layout_for_width(120), LayoutKind::ThreePane);
        assert_eq!(layout_for_width(100), LayoutKind::ThreePane);
        assert_eq!(layout_for_width(99), LayoutKind::TwoPane);
        assert_eq!(layout_for_width(60), LayoutKind::TwoPane);
        assert_eq!(layout_for_width(59), LayoutKind::Compact);
        assert_eq!(layout_for_width(0), LayoutKind::Compact);
    }

    /// Headless smoke test: render at each layout tier and confirm no panic
    /// (chunk-indexing or wrap math) reaches the user. Substitutes for an
    /// interactive resize: we can't drive a real terminal from a unit test,
    /// but we can prove the three code paths terminate normally.
    fn render_at(width: u16, height: u16, mutate: impl FnOnce(&mut App)) {
        use ratatui::{backend::TestBackend, Terminal};

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("create test terminal");
        let mut app = App::new(
            GhostConfig::default(),
            String::new(),
            PathBuf::from("/tmp/test.toml"),
        );
        mutate(&mut app);
        terminal.draw(|f| super::render(f, &app)).expect("render");
    }

    #[test]
    fn render_three_pane_at_120_cols_does_not_panic() {
        render_at(120, 30, |_| {});
    }

    #[test]
    fn render_two_pane_at_80_cols_does_not_panic() {
        render_at(80, 24, |_| {});
    }

    #[test]
    fn render_two_pane_with_preview_overlay_at_80_cols() {
        render_at(80, 24, |app| app.show_preview = true);
    }

    #[test]
    fn render_compact_at_50_cols_does_not_panic() {
        render_at(50, 24, |_| {});
    }

    #[test]
    fn render_compact_at_30_cols_does_not_panic() {
        render_at(30, 24, |_| {});
    }

    #[test]
    fn render_with_active_filter_does_not_panic() {
        render_at(80, 24, |app| {
            app.filter = Some("render".to_string());
        });
    }

    #[test]
    fn render_filter_input_prompt_does_not_panic() {
        render_at(80, 24, |app| {
            app.prompt = Some(Prompt::FilterInput);
            app.filter = Some("ren".to_string());
        });
    }
}
