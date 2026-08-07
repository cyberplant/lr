//! TUI rendering. Renders parsed log lines with virtual scrolling, a status
//! bar with live stats, and a command bar hint.

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::state::AppState;
use crate::plugin::Severity;

pub fn render(frame: &mut Frame, state: &mut AppState) {
    let area = frame.area();
    state.terminal_height = area.height;

    // [ log view | sidebar ] / [ status bar ] / [ command bar ]
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // main row
            Constraint::Length(1), // status bar
            Constraint::Length(1), // command bar
        ])
        .split(area);

    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(0)])
        .split(chunks[0]);

    render_log_view(frame, state, main[0]);
    render_status_bar(frame, state, chunks[1]);
    render_command_bar(frame, state, chunks[2]);
}

fn render_log_view(frame: &mut Frame, state: &AppState, area: ratatui::layout::Rect) {
    let title = if state.files.is_empty() {
        "lr — (stdin)".to_string()
    } else {
        format!("lr — {}", state.files.first().map(|p| p.display().to_string()).unwrap_or_default())
    };

    if state.lines.is_empty() {
        let view = Paragraph::new(vec![
            Line::from(Span::styled(
                "LR — Log Reader",
                Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan),
            )),
            Line::from(""),
            Line::from(if state.files.is_empty() && state.message.contains("no files") {
                "no files given — pass paths or --stdin"
            } else {
                "reading..."
            }),
            Line::from(""),
            Line::from("press ? for help, q to quit"),
        ])
        .block(Block::default().borders(Borders::ALL).title(title));
        frame.render_widget(view, area);
        return;
    }

    let visible_height = state.visible_height();
    let scroll = state.scroll.min(state.max_scroll());

    // Build visible lines, filtering by severity visibility and filter expr.
    let mut lines_rendered: Vec<Line> = Vec::with_capacity(visible_height);
    let mut idx = scroll;
    while lines_rendered.len() < visible_height && idx < state.lines.len() {
        let pl = &state.lines[idx];
        idx += 1;

        // Skip lines with hidden severity.
        if let Some(sev) = pl.severity
            && !state.severity_visible.is_visible(sev)
        {
            continue;
        }

        // Skip lines that don't match the active filter.
        if !state.passes_filter(pl) {
            continue;
        }

        lines_rendered.push(render_line(pl, area.width as usize, state.show_line_numbers, state));
    }

    // If we filtered out lines and haven't filled the view, keep going.
    while lines_rendered.len() < visible_height && idx < state.lines.len() {
        let pl = &state.lines[idx];
        idx += 1;
        if let Some(sev) = pl.severity
            && !state.severity_visible.is_visible(sev)
        {
            continue;
        }
        if !state.passes_filter(pl) {
            continue;
        }
        lines_rendered.push(render_line(pl, area.width as usize, state.show_line_numbers, state));
    }

    let block = Block::default().borders(Borders::ALL).title(title);
    let paragraph = Paragraph::new(lines_rendered).block(block);
    frame.render_widget(paragraph, area);
}

fn render_line(
    pl: &crate::pipeline::parser::ParsedLine,
    _width: usize,
    show_line_no: bool,
    state: &AppState,
) -> Line<'static> {
    let color = match pl.severity {
        Some(Severity::Error) => Color::Red,
        Some(Severity::Warn) => Color::Yellow,
        Some(Severity::Info) => Color::Cyan,
        Some(Severity::Debug) => Color::DarkGray,
        Some(Severity::Trace) => Color::DarkGray,
        None => Color::Reset,
    };

    // Build the text spans, highlighting search matches if active.
    let text_spans = if let Some(ref search) = state.search {
        highlight_matches(&pl.raw, search, color)
    } else {
        vec![Span::styled(pl.raw.clone(), Style::default().fg(color))]
    };

    if show_line_no {
        let mut spans = vec![Span::styled(
            format!("{:>6} ", pl.line_no),
            Style::default().fg(Color::DarkGray),
        )];
        spans.extend(text_spans);
        Line::from(spans)
    } else {
        Line::from(text_spans)
    }
}

/// Split text into spans, highlighting regex matches in black-on-yellow.
fn highlight_matches(text: &str, search: &crate::search::Search, base_color: Color) -> Vec<Span<'static>> {
    let matches = search.find_iter(text);
    if matches.is_empty() {
        return vec![Span::styled(text.to_string(), Style::default().fg(base_color))];
    }

    let mut spans = Vec::with_capacity(matches.len() * 2 + 1);
    let mut last_end = 0;
    for (start, end) in matches {
        if start > last_end {
            spans.push(Span::styled(
                text[last_end..start].to_string(),
                Style::default().fg(base_color),
            ));
        }
        spans.push(Span::styled(
            text[start..end].to_string(),
            Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
        last_end = end;
    }
    if last_end < text.len() {
        spans.push(Span::styled(
            text[last_end..].to_string(),
            Style::default().fg(base_color),
        ));
    }
    spans
}

fn render_status_bar(frame: &mut Frame, state: &AppState, area: ratatui::layout::Rect) {
    let follow_indicator = if state.follow { "FOLLOW" } else { "  --  " };

    // File processing indicator: "Processing: X%" while head reader is
    // running, "Loaded" when done.
    let processing_str = if state.progress.head_done() {
        "Loaded".to_string()
    } else {
        format!("Processing: {:.0}%", state.progress.fraction() * 100.0)
    };

    // Scroll position: use the line_no of the first visible line (if any)
    // and the estimated/exact total. Show "(est!)" suffix when estimated.
    let (pos_str, pos_color) = if state.lines.is_empty() {
        ("0/0".to_string(), Color::White)
    } else {
        let first_visible = &state.lines[state.scroll.min(state.lines.len() - 1)];
        let total = if state.progress.lines_estimated() {
            state.progress.estimated_total_lines()
        } else {
            state.lines.len() as u64
        };
        let current = first_visible.line_no;
        if state.progress.lines_estimated() {
            (format!("{}/{} (est!)", current, total), Color::Yellow)
        } else {
            (format!("{}/{}", current, total), Color::White)
        }
    };

    let sev_flags = format!(
        "{}{}{}{}{}",
        if state.severity_visible.error { "E" } else { "-" },
        if state.severity_visible.warn { "W" } else { "-" },
        if state.severity_visible.info { "I" } else { "-" },
        if state.severity_visible.debug { "D" } else { "-" },
        if state.severity_visible.trace { "T" } else { "-" },
    );

    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", follow_indicator),
            Style::default()
                .fg(if state.follow { Color::Black } else { Color::Reset })
                .bg(if state.follow { Color::Green } else { Color::Reset })
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{:.0} L/s", state.stats.lines_per_sec),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw("  |  "),
        Span::styled(sev_flags, Style::default().fg(Color::White)),
        Span::raw("  |  "),
        Span::styled(processing_str, Style::default().fg(Color::Magenta)),
        Span::raw("  |  "),
        Span::styled(pos_str, Style::default().fg(pos_color)),
        // Show search match count if a search is active.
        if !state.search_matches.is_empty() {
            Span::raw("  |  ")
        } else {
            Span::raw("")
        },
        if !state.search_matches.is_empty() {
            Span::styled(
                format!("match {}/{}", state.search_cursor + 1, state.search_matches.len()),
                Style::default().fg(Color::Yellow),
            )
        } else {
            Span::raw("")
        },
    ]);

    let bar = Paragraph::new(line).style(Style::default().bg(Color::Blue));
    frame.render_widget(bar, area);
}

fn render_command_bar(frame: &mut Frame, state: &AppState, area: ratatui::layout::Rect) {
    use crate::app::state::InputMode;

    let line = match state.input_mode {
        InputMode::Normal => {
            // Show hint line or message.
            if !state.message.is_empty() {
                Line::from(Span::styled(
                    format!(" {}", state.message),
                    Style::default().fg(Color::Yellow),
                ))
            } else {
                Line::from(Span::styled(
                    " : command  / search  f follow  l line#  HOME/END  1-5 severity  n/N match  Esc clear  ? help  q quit",
                    Style::default().fg(Color::DarkGray),
                ))
            }
        }
        InputMode::Search => {
            Line::from(vec![
                Span::styled("/", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(state.input_buffer.as_str(), Style::default().fg(Color::White)),
                Span::styled("_", Style::default().fg(Color::Gray).add_modifier(Modifier::SLOW_BLINK)),
            ])
        }
        InputMode::Command => {
            Line::from(vec![
                Span::styled(":", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(state.input_buffer.as_str(), Style::default().fg(Color::White)),
                Span::styled("_", Style::default().fg(Color::Gray).add_modifier(Modifier::SLOW_BLINK)),
            ])
        }
    };
    frame.render_widget(Paragraph::new(line), area);
}
