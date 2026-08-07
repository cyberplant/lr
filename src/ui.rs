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

    // Build visible lines, filtering by severity visibility.
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

        lines_rendered.push(render_line(pl, area.width as usize));
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
        lines_rendered.push(render_line(pl, area.width as usize));
    }

    let block = Block::default().borders(Borders::ALL).title(title);
    let paragraph = Paragraph::new(lines_rendered).block(block);
    frame.render_widget(paragraph, area);
}

fn render_line(pl: &crate::pipeline::parser::ParsedLine, _width: usize) -> Line<'static> {
    let (badge_char, badge_color) = match pl.severity {
        Some(Severity::Error) => ("E", Color::Red),
        Some(Severity::Warn) => ("W", Color::Yellow),
        Some(Severity::Info) => ("I", Color::Cyan),
        Some(Severity::Debug) => ("D", Color::DarkGray),
        Some(Severity::Trace) => ("T", Color::DarkGray),
        None => (" ", Color::Reset),
    };

    let line_no_str = format!("{:>6} ", pl.line_no);

    Line::from(vec![
        Span::styled(line_no_str, Style::default().fg(Color::DarkGray)),
        Span::raw("["),
        Span::styled(badge_char, Style::default().fg(badge_color).add_modifier(Modifier::BOLD)),
        Span::raw("] "),
        Span::raw(pl.raw.clone()),
    ])
}

fn render_status_bar(frame: &mut Frame, state: &AppState, area: ratatui::layout::Rect) {
    let follow_indicator = if state.follow { "FOLLOW" } else { "  --  " };
    let pos = if state.lines.is_empty() {
        "0/0".to_string()
    } else {
        format!("{}/{}", state.scroll + 1, state.lines.len())
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
            format!("{} lines", state.stats.total_lines),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw("  |  "),
        Span::styled(
            format!("{:.0} L/s", state.stats.lines_per_sec),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw("  |  "),
        Span::styled(sev_flags, Style::default().fg(Color::White)),
        Span::raw("  |  "),
        Span::styled(pos, Style::default().fg(Color::White)),
    ]);

    let bar = Paragraph::new(line).style(Style::default().bg(Color::DarkGray));
    frame.render_widget(bar, area);
}

fn render_command_bar(frame: &mut Frame, _state: &AppState, area: ratatui::layout::Rect) {
    let hint = Line::from(Span::styled(
        " : command  / search  f follow  HOME/END  1-5 severity  ? help  q quit",
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(Paragraph::new(hint), area);
}
