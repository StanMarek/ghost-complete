use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::app::{App, EditState, Focus};
use super::fields::{self, FieldMeta, ReloadBehavior, SECTIONS};
use super::preview;

/// Top-level render function. Draws the three-pane layout + footer.
pub fn render(frame: &mut Frame, app: &App) {
    let size = frame.area();

    // Vertical split: main area + footer
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(2)])
        .split(size);

    let main_area = vertical[0];
    let footer_area = vertical[1];

    // Horizontal split: sections | fields | preview
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(16),
            Constraint::Fill(1),
            Constraint::Length(50),
        ])
        .split(main_area);

    render_sections(frame, app, horizontal[0]);
    render_fields(frame, app, horizontal[1]);
    preview::render_preview(frame, app, horizontal[2]);
    render_footer(frame, app, footer_area);
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
    let section_fields = app.current_section_fields();

    let block = Block::default()
        .title(format!(" {} ", fields::section_label(section)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if section_fields.is_empty() {
        return;
    }

    let mut lines: Vec<Line> = Vec::new();

    for (i, field) in section_fields.iter().enumerate() {
        let is_selected = app.focus == Focus::Fields && i == app.field_idx;
        let value = app.field_value(field);

        // Field name line
        let mut name_spans: Vec<Span> = Vec::new();

        let name_style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Yellow)
        };

        name_spans.push(Span::styled(format!("  {}", field.key), name_style));

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
    let field_errors = visible_errors(app, &section_fields);

    for (key, msg) in &field_errors {
        lines.push(Line::from(Span::styled(
            format!("  Error ({key}): {msg}"),
            Style::default().fg(Color::Red),
        )));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let mode_str = match app.edit_state {
        EditState::None => "Navigate",
        EditState::Text { .. } => "Edit",
    };

    let mut spans: Vec<Span> = vec![
        Span::styled(" Tab", Style::default().fg(Color::Cyan)),
        Span::raw(" switch  "),
        Span::styled("Enter", Style::default().fg(Color::Cyan)),
        Span::raw(" edit  "),
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
        Span::styled(
            format!("Section: {}", fields::section_label(app.current_section())),
            Style::default().fg(Color::DarkGray),
        ),
    ];

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
}
