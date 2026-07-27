use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{App, DetailView, SearchMode, Theme};

fn mode_name(mode: SearchMode) -> &'static str {
    match mode {
        SearchMode::Search => "SEARCH",
        SearchMode::Locate => "LOCATE",
        SearchMode::Which => "WHICH",
    }
}

fn theme_colors(theme: Theme) -> ThemeColors {
    match theme {
        Theme::TokyoNight | Theme::Nord => ThemeColors {
            bg: Color::Rgb(30, 30, 46),
            fg: Color::Rgb(220, 220, 220),
            header_bg: Color::Rgb(40, 40, 60),
            header_fg: Color::Rgb(220, 220, 220),
            mode_fg: Color::Rgb(136, 192, 208),
            selected_bg: Color::Rgb(50, 50, 70),
            selected_fg: Color::Rgb(240, 240, 240),
            selected_modifier: Modifier::BOLD,
            attr_fg: Color::Rgb(163, 216, 163),
            desc_fg: Color::Rgb(150, 150, 170),
            status_fg: Color::Rgb(120, 120, 140),
            accent_fg: Color::Rgb(136, 192, 208),
            detail_label_fg: Color::Rgb(136, 192, 208),
            detail_value_fg: Color::Rgb(220, 220, 220),
            overlay_bg: Color::Rgb(20, 20, 36),
            overlay_border: Color::Rgb(80, 80, 120),
            toast_bg: Color::Rgb(50, 50, 70),
            toast_fg: Color::Rgb(220, 220, 220),
            spinner_fg: Color::Rgb(240, 240, 240),
            pinned_fg: Color::Rgb(240, 240, 240),
        },
        Theme::CatppuccinMocha => ThemeColors {
            bg: Color::Rgb(30, 30, 46),
            fg: Color::Rgb(205, 214, 244),
            header_bg: Color::Rgb(49, 50, 68),
            header_fg: Color::Rgb(205, 214, 244),
            mode_fg: Color::Rgb(166, 227, 240),
            selected_bg: Color::Rgb(69, 71, 90),
            selected_fg: Color::Rgb(245, 224, 220),
            selected_modifier: Modifier::BOLD,
            attr_fg: Color::Rgb(166, 227, 240),
            desc_fg: Color::Rgb(166, 173, 200),
            status_fg: Color::Rgb(137, 140, 160),
            accent_fg: Color::Rgb(166, 227, 240),
            detail_label_fg: Color::Rgb(166, 227, 240),
            detail_value_fg: Color::Rgb(205, 214, 244),
            overlay_bg: Color::Rgb(24, 24, 38),
            overlay_border: Color::Rgb(76, 77, 100),
            toast_bg: Color::Rgb(49, 50, 68),
            toast_fg: Color::Rgb(205, 214, 244),
            spinner_fg: Color::Rgb(245, 224, 220),
            pinned_fg: Color::Rgb(245, 224, 220),
        },
        Theme::Dracula => ThemeColors {
            bg: Color::Rgb(40, 40, 50),
            fg: Color::Rgb(220, 220, 220),
            header_bg: Color::Rgb(50, 50, 65),
            header_fg: Color::Rgb(220, 220, 220),
            mode_fg: Color::Rgb(189, 147, 249),
            selected_bg: Color::Rgb(60, 60, 80),
            selected_fg: Color::Rgb(248, 248, 242),
            selected_modifier: Modifier::BOLD,
            attr_fg: Color::Rgb(80, 250, 123),
            desc_fg: Color::Rgb(150, 150, 170),
            status_fg: Color::Rgb(120, 120, 140),
            accent_fg: Color::Rgb(189, 147, 249),
            detail_label_fg: Color::Rgb(189, 147, 249),
            detail_value_fg: Color::Rgb(248, 248, 242),
            overlay_bg: Color::Rgb(30, 30, 40),
            overlay_border: Color::Rgb(100, 100, 140),
            toast_bg: Color::Rgb(60, 60, 80),
            toast_fg: Color::Rgb(248, 248, 242),
            spinner_fg: Color::Rgb(248, 248, 242),
            pinned_fg: Color::Rgb(248, 248, 242),
        },
    }
}

struct ThemeColors {
    bg: Color,
    fg: Color,
    #[allow(dead_code)]
    header_bg: Color,
    #[allow(dead_code)]
    header_fg: Color,
    mode_fg: Color,
    selected_bg: Color,
    selected_fg: Color,
    selected_modifier: Modifier,
    attr_fg: Color,
    #[allow(dead_code)]
    desc_fg: Color,
    status_fg: Color,
    #[allow(dead_code)]
    accent_fg: Color,
    detail_label_fg: Color,
    detail_value_fg: Color,
    #[allow(dead_code)]
    overlay_bg: Color,
    #[allow(dead_code)]
    overlay_border: Color,
    toast_bg: Color,
    toast_fg: Color,
    spinner_fg: Color,
    pinned_fg: Color,
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

    let tc = theme_colors(app.theme);

    render_header(frame, chunks.first().copied().unwrap_or_else(ratatui::layout::Rect::default), app, &tc);
    render_body(frame, chunks.get(1).copied().unwrap_or_else(ratatui::layout::Rect::default), app, &tc);
    render_footer(frame, chunks.get(2).copied().unwrap_or_else(ratatui::layout::Rect::default), app, &tc);
    render_toasts(frame, app, &tc);

    if app.show_help {
        render_help_overlay(frame, size, &tc);
    }
}

fn render_header(frame: &mut Frame<'_>, area: ratatui::layout::Rect, app: &App, tc: &ThemeColors) {
    let mode_label = format!(" {} ", mode_name(app.mode));
    let mode_span = Span::styled(
        mode_label,
        Style::default().fg(tc.mode_fg).add_modifier(Modifier::BOLD),
    );

    let input_text = if app.input.is_empty() {
        format!("  {}...", mode_name(app.mode))
    } else {
        format!("  {}", app.input)
    };

    let input_line = Line::from(vec![mode_span, Span::raw(input_text)]);

    let paragraph = Paragraph::new(input_line)
        .block(Block::default().borders(Borders::ALL).title(" nixdex tui "))
        .style(Style::default().fg(tc.fg).bg(tc.bg));

    frame.render_widget(paragraph, area);
}

fn render_body(frame: &mut Frame<'_>, area: ratatui::layout::Rect, app: &App, tc: &ThemeColors) {
    if app.is_searching {
        render_loading(frame, area, tc);
        return;
    }

    if let Some(detail) = &app.detail {
        render_detail(frame, area, detail, tc);
    } else {
        render_results(frame, area, app, tc);
    }
}

fn render_loading(frame: &mut Frame<'_>, area: ratatui::layout::Rect, tc: &ThemeColors) {
    let lines = vec![Line::from(vec![Span::styled(
        " Searching... ",
        Style::default()
            .fg(tc.spinner_fg)
            .add_modifier(Modifier::BOLD),
    )])];
    let text = Text::from(lines);
    let paragraph = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title(" Search "))
        .style(Style::default().fg(tc.fg).bg(tc.bg))
        .alignment(Alignment::Center);

    frame.render_widget(paragraph, area);
}

fn render_results(frame: &mut Frame<'_>, area: ratatui::layout::Rect, app: &App, tc: &ThemeColors) {
    let items: Vec<ListItem> = app
        .results
        .iter()
        .enumerate()
        .map(|(i, result)| {
            let line = if app.search_name_only {
                Line::from(vec![Span::styled(
                    &result.attr,
                    Style::default().fg(tc.attr_fg),
                )])
            } else if app.search_json {
                Line::from(vec![Span::raw(format!(
                    "{}  {}  {}",
                    result.attr, result.name, result.description
                ))])
            } else {
                let attr_span = if i == app.selected {
                    Span::styled(
                        &result.attr,
                        Style::default()
                            .fg(tc.selected_fg)
                            .add_modifier(tc.selected_modifier),
                    )
                } else {
                    Span::styled(&result.attr, Style::default().fg(tc.attr_fg))
                };
                let name_span = Span::raw("  ");
                let name_val = Span::raw(&result.name);
                let desc_span = Span::raw("  ");
                let desc_val = Span::raw(&result.description);
                Line::from(vec![attr_span, name_span, name_val, desc_span, desc_val])
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
                .bg(tc.selected_bg)
                .fg(tc.selected_fg)
                .add_modifier(tc.selected_modifier),
        )
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, area, &mut list_state);
}

fn detail_label_style(tc: &ThemeColors) -> Style {
    Style::default()
        .fg(tc.detail_label_fg)
        .add_modifier(Modifier::BOLD)
}

fn detail_line(label: &'static str, value: String, tc: &ThemeColors) -> Line<'static> {
    Line::from(vec![
        Span::styled(label, detail_label_style(tc)),
        Span::raw(format!("  {}", value)),
    ])
}

fn render_detail(
    frame: &mut Frame<'_>,
    area: ratatui::layout::Rect,
    detail: &DetailView,
    tc: &ThemeColors,
) {
    let mut lines = Vec::new();
    lines.push(detail_line("Attribute:", detail.attr.clone(), tc));
    lines.push(detail_line("Name:", detail.name.clone(), tc));
    lines.push(detail_line("Description:", detail.description.clone(), tc));
    if let Some(path) = &detail.path {
        lines.push(detail_line("Path:", path.clone(), tc));
    }
    if let Some(size) = detail.size {
        lines.push(detail_line("Size:", format!("{} bytes", size), tc));
    }
    if let Some(license) = &detail.license {
        lines.push(detail_line("License:", license.clone(), tc));
    }
    if let Some(homepage) = &detail.homepage {
        lines.push(detail_line("Homepage:", homepage.clone(), tc));
    }
    if !detail.maintainers.is_empty() {
        lines.push(detail_line("Maintainers:", detail.maintainers.join(", "), tc));
    }
    if let Some(main_program) = &detail.main_program {
        lines.push(detail_line("Main program:", main_program.clone(), tc));
    }
    if detail.pinned {
        lines.push(Line::from(vec![
            Span::styled(
                "PINNED",
                Style::default()
                    .fg(tc.pinned_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  (press Space to unpin)"),
        ]));
    }

    let text = Text::from(lines);
    let paragraph = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title(" Details "))
        .style(Style::default().fg(tc.detail_value_fg).bg(tc.bg))
        .wrap(Wrap { trim: true });

    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame<'_>, area: ratatui::layout::Rect, app: &App, tc: &ThemeColors) {
    let result_count = app.result_count();
    let mode_label = mode_name(app.mode);
    let footer_text = format!(
        " {} | {} results | {} ",
        app.status_message, result_count, mode_label
    );

    let paragraph = Paragraph::new(footer_text)
        .block(Block::default().borders(Borders::TOP))
        .style(Style::default().fg(tc.status_fg).bg(tc.bg))
        .alignment(Alignment::Left);

    frame.render_widget(paragraph, area);
}

fn render_toasts(frame: &mut Frame<'_>, app: &App, tc: &ThemeColors) {
    if app.toasts.is_empty() {
        return;
    }

    let toast_area = ratatui::layout::Rect::new(
        frame.size().width.saturating_sub(40),
        frame.size().height.saturating_sub(3),
        40,
        1,
    );

    let Some(latest) = app.toasts.last() else {
        return;
    };
    let paragraph = Paragraph::new(latest.message.as_str())
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(tc.toast_fg).bg(tc.toast_bg));

    frame.render_widget(paragraph, toast_area);
}

fn centered_rect(area: ratatui::layout::Rect, percent_x: u16, percent_y: u16) -> ratatui::layout::Rect {
    let vertical = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Percentage((100 - percent_y) / 2),
            ratatui::layout::Constraint::Percentage(percent_y),
            ratatui::layout::Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    let middle = match vertical.get(1) {
        Some(rect) => *rect,
        None => return area,
    };

    match ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Horizontal)
        .constraints([
            ratatui::layout::Constraint::Percentage((100 - percent_x) / 2),
            ratatui::layout::Constraint::Percentage(percent_x),
            ratatui::layout::Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(middle)
        .get(1)
    {
        Some(rect) => *rect,
        None => area,
    }
}

fn render_help_overlay(frame: &mut Frame<'_>, area: ratatui::layout::Rect, tc: &ThemeColors) {
    let help_text = [
        " nixdex TUI Help ",
        "",
        "  /            Focus search input",
        "  Tab          Switch search mode (Search/Locate/Which)",
        "  Enter        Submit search",
        "  Esc          Clear input / exit help",
        "  Up/Down      Navigate results",
        "  s            Cycle sort order",
        "  f            Toggle fuzzy search",
        "  r            Toggle regex search",
        "  c            Toggle case sensitivity",
        "  e            Toggle exact match",
        "  n            Toggle name-only display",
        "  j            Toggle JSON output",
        "  ?            Toggle this help",
        "  q            Quit",
        "",
        " Press any key or Esc to continue.",
    ];
    let paragraph = Paragraph::new(help_text.iter().map(|line| Line::raw(*line)).collect::<Vec<_>>())
        .block(Block::default().borders(Borders::ALL).title(" Help "))
        .style(Style::default().fg(tc.fg).bg(tc.overlay_bg));

    let overlay_area = centered_rect(area, 60, 70);
    frame.render_widget(paragraph, overlay_area);
}
