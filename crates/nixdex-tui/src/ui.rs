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
        // Tokyo Night: https://github.com/enkia/tokyo-night-vscode-theme
        Theme::TokyoNight => ThemeColors {
            bg: Color::Rgb(26, 27, 38),
            fg: Color::Rgb(192, 202, 245),
            header_bg: Color::Rgb(36, 40, 59),
            header_fg: Color::Rgb(192, 202, 245),
            mode_fg: Color::Rgb(122, 162, 247),
            selected_bg: Color::Rgb(41, 46, 66),
            selected_fg: Color::Rgb(192, 202, 245),
            selected_modifier: Modifier::BOLD,
            attr_fg: Color::Rgb(158, 206, 106),
            desc_fg: Color::Rgb(86, 95, 137),
            status_fg: Color::Rgb(86, 95, 137),
            accent_fg: Color::Rgb(122, 162, 247),
            detail_label_fg: Color::Rgb(122, 162, 247),
            detail_value_fg: Color::Rgb(192, 202, 245),
            overlay_bg: Color::Rgb(22, 22, 30),
            overlay_border: Color::Rgb(59, 66, 97),
            toast_bg: Color::Rgb(36, 40, 59),
            toast_fg: Color::Rgb(192, 202, 245),
            spinner_fg: Color::Rgb(224, 175, 104),
            pinned_fg: Color::Rgb(187, 154, 247),
        },
        // Nord: https://www.nordtheme.com/docs/colors-and-palette
        Theme::Nord => ThemeColors {
            bg: Color::Rgb(46, 52, 64),
            fg: Color::Rgb(216, 222, 233),
            header_bg: Color::Rgb(59, 66, 82),
            header_fg: Color::Rgb(236, 239, 244),
            mode_fg: Color::Rgb(136, 192, 208),
            selected_bg: Color::Rgb(67, 76, 94),
            selected_fg: Color::Rgb(236, 239, 244),
            selected_modifier: Modifier::BOLD,
            attr_fg: Color::Rgb(163, 190, 140),
            desc_fg: Color::Rgb(129, 161, 193),
            status_fg: Color::Rgb(76, 86, 106),
            accent_fg: Color::Rgb(143, 188, 187),
            detail_label_fg: Color::Rgb(136, 192, 208),
            detail_value_fg: Color::Rgb(229, 233, 240),
            overlay_bg: Color::Rgb(59, 66, 82),
            overlay_border: Color::Rgb(94, 129, 172),
            toast_bg: Color::Rgb(67, 76, 94),
            toast_fg: Color::Rgb(236, 239, 244),
            spinner_fg: Color::Rgb(235, 203, 139),
            pinned_fg: Color::Rgb(180, 142, 173),
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
    let size = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            // The header draws a bordered block, so it needs three rows: the top
            // border, the input line, and the bottom border.
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(size);

    let tc = theme_colors(app.theme);

    render_header(
        frame,
        chunks
            .first()
            .copied()
            .unwrap_or_else(ratatui::layout::Rect::default),
        app,
        &tc,
    );
    render_body(
        frame,
        chunks
            .get(1)
            .copied()
            .unwrap_or_else(ratatui::layout::Rect::default),
        app,
        &tc,
    );
    render_footer(
        frame,
        chunks
            .get(2)
            .copied()
            .unwrap_or_else(ratatui::layout::Rect::default),
        app,
        &tc,
    );
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

/// Render one result as a single-line JSON object, so the Ctrl+J toggle produces
/// the same shape a `--json` run would, not a re-spaced text row.
fn json_row(result: &crate::app::SearchResult) -> String {
    let mut obj = sonic_rs::json!({
        "attr": result.attr,
        "name": result.name,
        "description": result.description,
    });
    if let Some(ref path) = result.path {
        obj.insert("path", sonic_rs::Value::copy_str(path));
    }
    if let Some(size) = result.size {
        obj.insert("size", sonic_rs::Value::from(size));
    }
    if let Some(ref license) = result.license {
        obj.insert("license", sonic_rs::Value::copy_str(license));
    }
    if let Some(ref homepage) = result.homepage {
        obj.insert("homepage", sonic_rs::Value::copy_str(homepage));
    }
    if !result.maintainers.is_empty()
        && let Ok(val) = sonic_rs::to_value(&result.maintainers)
    {
        obj.insert("maintainers", val);
    }
    if let Some(ref mp) = result.main_program {
        obj.insert("main_program", sonic_rs::Value::copy_str(mp));
    }
    sonic_rs::to_string(&obj).unwrap_or_else(|_| String::new())
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
                Line::from(vec![Span::raw(json_row(result))])
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
        lines.push(detail_line(
            "Maintainers:",
            detail.maintainers.join(", "),
            tc,
        ));
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
        frame.area().width.saturating_sub(40),
        frame.area().height.saturating_sub(3),
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

fn centered_rect(
    area: ratatui::layout::Rect,
    percent_x: u16,
    percent_y: u16,
) -> ratatui::layout::Rect {
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
    // Every entry here must have a live handler in `tui.rs`. Plain printable
    // characters are search input, so each command key carries a modifier.
    let help_text = [
        " nixdex TUI Help ",
        "",
        "  type         Search -- every plain character goes to the query,",
        "               including / : and ?",
        "  Tab          Switch search mode (Search/Locate/Which)",
        "  Enter        Submit the search now",
        "  Esc          Clear the search",
        "  Up/Down      Move the selection",
        "  PgUp/PgDn    Move the selection by a page",
        "  Home/End     Jump to the first or last result",
        "  Ctrl+D       Pin or unpin the detail pane",
        "  Ctrl+A       Expand or collapse every result",
        "  Ctrl+Y       Copy the selected attribute",
        "  Ctrl+E       Copy a nix-env install command",
        "  Ctrl+P       Copy a nix profile install command",
        "  Ctrl+J       Toggle JSON output",
        "  Ctrl+R       Refresh",
        "  Ctrl+T       Cycle the theme",
        "  Ctrl+H       Toggle this help",
        "  Ctrl+C       Quit",
        "",
        " Press any key or Esc to continue.",
    ];
    let paragraph = Paragraph::new(
        help_text
            .iter()
            .map(|line| Line::raw(*line))
            .collect::<Vec<_>>(),
    )
    .block(Block::default().borders(Borders::ALL).title(" Help "))
    .style(Style::default().fg(tc.fg).bg(tc.overlay_bg));

    let overlay_area = centered_rect(area, 60, 70);
    frame.render_widget(paragraph, overlay_area);
}

#[cfg(test)]
mod json_row_tests {
    use super::json_row;
    use crate::app::SearchResult;

    #[test]
    fn ctrl_j_rows_are_real_json_not_re_spaced_columns() {
        let result = SearchResult {
            attr: "hello".to_string(),
            name: "hello-2.12".to_string(),
            description: "a greeting".to_string(),
            path: Some("/bin/hello".to_string()),
            size: Some(42),
            license: None,
            homepage: None,
            maintainers: Vec::new(),
            main_program: Some("hello".to_string()),
        };

        let row = json_row(&result);

        assert!(row.starts_with('{') && row.ends_with('}'), "{row:?}");
        assert!(row.contains("\"attr\":\"hello\""), "{row:?}");
        assert!(row.contains("\"size\":42"), "{row:?}");
        assert!(row.contains("\"main_program\":\"hello\""), "{row:?}");
        assert!(
            !row.contains("\"license\""),
            "absent fields stay out: {row:?}"
        );
    }
}
