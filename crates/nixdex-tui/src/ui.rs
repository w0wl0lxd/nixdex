use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, DetailView, SearchMode, SearchResult};

fn mode_name(mode: SearchMode) -> &'static str {
    match mode {
        SearchMode::Search => "SEARCH",
        SearchMode::Locate => "LOCATE",
        SearchMode::Which => "WHICH",
    }
}

pub fn render(frame: &mut Frame<'_>, app: &App) {
    let size = frame.size();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(size);

    render_header(frame, chunks[0], app);
    render_body(frame, chunks[1], app);
    render_footer(frame, chunks[2], app);
}

fn render_header(frame: &mut Frame<'_>, area: ratatui::layout::Rect, app: &App) {
    let mode_label = format!(" {} ", mode_name(app.mode));
    let mode_span = Span::styled(
        mode_label,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );

    let input_text = if app.input.is_empty() {
        format!("  {}...", mode_name(app.mode))
    } else {
        format!("  {}", app.input)
    };

    let input_line = Line::from(vec![mode_span, Span::raw(input_text)]);

    let paragraph = Paragraph::new(input_line)
        .block(Block::default().borders(Borders::ALL).title(" nixdex tui "))
        .style(Style::default().fg(Color::White));

    frame.render_widget(paragraph, area);
}

fn render_body(frame: &mut Frame<'_>, area: ratatui::layout::Rect, app: &App) {
    if let Some(detail) = &app.detail {
        render_detail(frame, area, detail);
    } else {
        render_results(frame, area, app);
    }
}

fn render_results(frame: &mut Frame<'_>, area: ratatui::layout::Rect, app: &App) {
    let items: Vec<ListItem> = app
        .results
        .iter()
        .enumerate()
        .map(|(i, result)| {
            let line = if app.search_name_only {
                Line::from(vec![Span::raw(&result.attr)])
            } else if app.search_json {
                Line::from(vec![Span::raw(format!(
                    "{}  {}  {}",
                    result.attr, result.name, result.description
                ))])
            } else {
                let attr_span = if i == app.selected {
                    Span::styled(
                        &result.attr,
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::raw(&result.attr)
                };
                let name_span = Span::raw(format!("  {}", result.name));
                let desc_span = Span::raw(format!("  {}", result.description));
                Line::from(vec![attr_span, name_span, desc_span])
            };
            ListItem::new(line)
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(app.selected));

    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_detail(frame: &mut Frame<'_>, area: ratatui::layout::Rect, detail: &DetailView) {
    let lines = {
        let mut lines = Vec::new();
        lines.push(Line::from(vec![
            Span::styled(
                "Attribute:",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("  {}", detail.attr)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                "Name:",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("  {}", detail.name)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                "Description:",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("  {}", detail.description)),
        ]));
        if let Some(path) = &detail.path {
            lines.push(Line::from(vec![
                Span::styled(
                    "Path:",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  {}", path)),
            ]));
        }
        if let Some(size) = detail.size {
            lines.push(Line::from(vec![
                Span::styled(
                    "Size:",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  {} bytes", size)),
            ]));
        }
        if let Some(license) = &detail.license {
            lines.push(Line::from(vec![
                Span::styled(
                    "License:",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  {}", license)),
            ]));
        }
        if let Some(homepage) = &detail.homepage {
            lines.push(Line::from(vec![
                Span::styled(
                    "Homepage:",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  {}", homepage)),
            ]));
        }
        if !detail.maintainers.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(
                    "Maintainers:",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  {}", detail.maintainers.join(", "))),
            ]));
        }
        if let Some(main_program) = &detail.main_program {
            lines.push(Line::from(vec![
                Span::styled(
                    "Main program:",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  {}", main_program)),
            ]));
        }
        lines
    };

    let text = Text::from(lines);
    let paragraph = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title(" Details "))
        .style(Style::default().fg(Color::White))
        .wrap(Wrap { trim: true });

    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame<'_>, area: ratatui::layout::Rect, app: &App) {
    let result_count = app.result_count();
    let mode_label = mode_name(app.mode);
    let footer_text = format!(
        " {} | {} results | {} ",
        app.status_message,
        result_count,
        mode_label
    );

    let paragraph = Paragraph::new(footer_text)
        .block(Block::default().borders(Borders::TOP))
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Left);

    frame.render_widget(paragraph, area);
}