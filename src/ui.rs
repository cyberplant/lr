//! TUI rendering. Renders parsed log lines with virtual scrolling, a status
//! bar with live stats, and a command bar hint.

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::state::AppState;
use crate::plugin::Severity;

/// Render a line's text with control characters shown as hex escapes.
/// Control characters (ESC, DEL, and other C0 codes except \t) are
/// rendered as `<XX>` in dark yellow to make them visible and prevent
/// terminal corruption from escape sequences in log file content.
/// Returns a vector of spans for the text portion (no line number).
fn render_text_spans(
    text: &str,
    base_color: Color,
    search: Option<&crate::search::Search>,
    is_cursor: bool,
) -> Vec<Span<'static>> {
    let cursor_bg = if is_cursor { Color::Blue } else { Color::Reset };

    if let Some(search) = search {
        // When search is active, we need to handle both match highlighting
        // and control char sanitization. Build the sanitized spans first,
        // then apply search highlighting on top.
        return highlight_matches(text, search, base_color, is_cursor);
    }

    // No search: build spans with control char hex escapes.
    let mut spans = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if is_control_char(c) {
            // Flush current text.
            if !current.is_empty() {
                spans.push(Span::styled(
                    std::mem::take(&mut current),
                    Style::default().fg(base_color).bg(cursor_bg),
                ));
            }
            // Push the hex escape in a distinct color.
            spans.push(Span::styled(
                format!("<{:02X}>", c as u32),
                Style::default().fg(Color::Yellow).bg(cursor_bg),
            ));
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        spans.push(Span::styled(current, Style::default().fg(base_color).bg(cursor_bg)));
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), Style::default().bg(cursor_bg)));
    }
    spans
}

/// Returns true if `c` is a terminal control character that should be
/// sanitized (shown as hex) to prevent terminal corruption.
pub fn is_control_char(c: char) -> bool {
    let code = c as u32;
    code == 0x1b || (code < 0x20 && code != 0x09 && code != 0x0a && code != 0x0d) || code == 0x7f
}

/// Sanitize a string for safe terminal output: replace control characters
/// with `<XX>` hex escape representations. Used by the REPL to prevent
/// terminal corruption when displaying log lines in non-TTY mode.
pub fn sanitize_for_terminal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if is_control_char(c) {
            out.push_str(&format!("<{:02X}>", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

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

fn render_log_view(frame: &mut Frame, state: &mut AppState, area: ratatui::layout::Rect) {
    let title = if state.files.is_empty() {
        "LR — (stdin)".to_string()
    } else {
        format!("LR — {}", state.files.first().map(|p| p.display().to_string()).unwrap_or_default())
    };

    if state.store.is_empty() {
        let view = Paragraph::new(vec![
            Line::from(Span::styled(
                "LR — Log Reader",
                Style::default().add_modifier(Modifier::BOLD).fg(Color::Yellow).bg(Color::Blue),
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
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(
                    title,
                    Style::default().fg(Color::Yellow).bg(Color::Blue).add_modifier(Modifier::BOLD),
                ))
                .border_style(Style::default().fg(Color::Blue)),
        );
        frame.render_widget(view, area);
        return;
    }

    let visible_height = state.visible_height();
    let scroll = state.scroll.min(state.max_scroll());
    let cursor_idx = state.cursor.min(state.store.len().saturating_sub(1));
    // Inner width of the log view (excluding left and right borders).
    let inner_width = area.width.saturating_sub(2) as usize;

    // Build visible lines, filtering by severity visibility and filter expr.
    // Lines are read on demand from the store (store.get takes &mut self).
    let mut lines_rendered: Vec<Line> = Vec::with_capacity(visible_height);
    let mut idx = scroll;
    while lines_rendered.len() < visible_height && idx < state.store.len() {
        let pl = match state.store.get(idx) {
            Some(pl) => pl,
            None => { idx += 1; continue; }
        };
        let is_cursor = idx == cursor_idx;
        idx += 1;

        // Skip lines with hidden severity.
        if let Some(sev) = pl.severity
            && !state.severity_visible.is_visible(sev)
        {
            continue;
        }

        // Skip lines that don't match the active filter.
        if !state.passes_filter(&pl) {
            continue;
        }

        lines_rendered.push(render_line(&pl, inner_width, state.show_line_numbers, state, is_cursor));
    }

    // If we filtered out lines and haven't filled the view, keep going.
    while lines_rendered.len() < visible_height && idx < state.store.len() {
        let pl = match state.store.get(idx) {
            Some(pl) => pl,
            None => { idx += 1; continue; }
        };
        let is_cursor = idx == cursor_idx;
        idx += 1;
        if let Some(sev) = pl.severity
            && !state.severity_visible.is_visible(sev)
        {
            continue;
        }
        if !state.passes_filter(&pl) {
            continue;
        }
        lines_rendered.push(render_line(&pl, inner_width, state.show_line_numbers, state, is_cursor));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            title,
            Style::default().fg(Color::Yellow).bg(Color::Blue).add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(Color::Blue));
    let paragraph = Paragraph::new(lines_rendered).block(block);
    frame.render_widget(paragraph, area);
}

fn render_line(
    pl: &crate::pipeline::parser::ParsedLine,
    inner_width: usize,
    show_line_no: bool,
    state: &AppState,
    is_cursor: bool,
) -> Line<'static> {
    let color = match pl.severity {
        Some(Severity::Error) => Color::Red,
        Some(Severity::Warn) => Color::Yellow,
        Some(Severity::Info) => Color::Cyan,
        Some(Severity::Debug) => Color::DarkGray,
        Some(Severity::Trace) => Color::DarkGray,
        None => Color::Reset,
    };

    // Cursor line: blue background, white text.
    let cursor_bg = if is_cursor { Color::Blue } else { Color::Reset };
    let cursor_fg = if is_cursor { Color::White } else { color };

    // Build the text spans, with control char hex escapes and search
    // match highlighting.
    let text_spans = render_text_spans(&pl.raw, cursor_fg, state.search.as_ref(), is_cursor);

    let mut spans = Vec::new();
    if show_line_no {
        spans.push(Span::styled(
            format!("{:>6} ", pl.line_no),
            Style::default().fg(Color::DarkGray).bg(cursor_bg),
        ));
    }
    spans.extend(text_spans);

    // Pad the cursor line to fill the full inner width with the cursor
    // background color, so the highlight bar spans the entire line.
    if is_cursor {
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        let pad = inner_width.saturating_sub(used);
        if pad > 0 {
            spans.push(Span::styled(
                " ".repeat(pad),
                Style::default().bg(cursor_bg),
            ));
        }
    }

    Line::from(spans)
}

/// Split text into spans, highlighting regex matches in black-on-yellow.
fn highlight_matches(text: &str, search: &crate::search::Search, base_color: Color, is_cursor: bool) -> Vec<Span<'static>> {
    let cursor_bg = if is_cursor { Color::Blue } else { Color::Reset };
    let matches = search.find_iter(text);
    if matches.is_empty() {
        // No matches — still sanitize control chars.
        return render_text_spans(text, base_color, None, is_cursor);
    }

    // Helper: push sanitized spans for a text segment with the given style.
    let push_sanitized = |spans: &mut Vec<Span<'static>>, segment: &str, fg: Color, bg: Color| {
        let mut current = String::new();
        for c in segment.chars() {
            if is_control_char(c) {
                if !current.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut current), Style::default().fg(fg).bg(bg)));
                }
                spans.push(Span::styled(
                    format!("<{:02X}>", c as u32),
                    Style::default().fg(Color::Yellow).bg(bg),
                ));
            } else {
                current.push(c);
            }
        }
        if !current.is_empty() {
            spans.push(Span::styled(current, Style::default().fg(fg).bg(bg)));
        }
    };

    let mut spans = Vec::with_capacity(matches.len() * 2 + 1);
    let mut last_end = 0;
    for (start, end) in matches {
        if start > last_end {
            push_sanitized(&mut spans, &text[last_end..start], base_color, cursor_bg);
        }
        // Search match highlighting takes priority over cursor bg.
        // Match text is shown as-is (user typed the pattern).
        spans.push(Span::styled(
            text[start..end].to_string(),
            Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
        last_end = end;
    }
    if last_end < text.len() {
        push_sanitized(&mut spans, &text[last_end..], base_color, cursor_bg);
    }
    spans
}

fn render_status_bar(frame: &mut Frame, state: &mut AppState, area: ratatui::layout::Rect) {
    let follow_indicator = if state.follow { "FOLLOW" } else { "  --  " };

    // File processing indicator: "Processing: X%" while head reader is
    // running, "Loaded" when done. Includes the current line rate.
    let processing_str = if state.progress.head_done() {
        format!("Loaded {:.0} L/s", state.stats.lines_per_sec)
    } else {
        format!("Processing: {:.0}% {:.0} L/s", state.progress.fraction() * 100.0, state.stats.lines_per_sec)
    };

    // Position: show the cursor line number and total.
    // The count is "estimated" only while the head reader is still running
    // AND we haven't yet loaded as many lines as the estimate. Once we have
    // all lines loaded, the count is exact regardless of the estimated flag.
    let count_is_exact = state.progress.head_done()
        || !state.progress.lines_estimated()
        || (state.progress.estimated_total_lines() > 0
            && state.store.len() as u64 >= state.progress.estimated_total_lines());
    let (pos_str, pos_color) = if state.store.is_empty() {
        ("0/0".to_string(), Color::White)
    } else {
        let cursor_idx = state.cursor.min(state.store.len() - 1);
        let cursor_line = state.store.get(cursor_idx);
        let total = if count_is_exact {
            state.store.len() as u64
        } else {
            state.progress.estimated_total_lines()
        };
        let current = cursor_line.as_ref().map(|l| l.line_no).unwrap_or(0);
        if count_is_exact {
            let base = format!("{}/{}", current, total);
            if state.filtering_active() {
                let vis = state.visible_count();
                if vis == 0 {
                    ("-- Nothing to display --".to_string(), Color::DarkGray)
                } else {
                    let rank = state.visible_rank(state.cursor.min(state.store.len() - 1));
                    (format!("{} {}/{} (filtered)", base, rank + 1, vis), Color::White)
                }
            } else {
                (base, Color::White)
            }
        } else {
            (format!("{}/{} (est!)", current, total), Color::Yellow)
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

    // Sparkline: 5-character rate trend graph.
    let sparkline = state.stats.sparkline(5);

    // Left side of the status bar.
    let mut left_spans: Vec<Span> = vec![
        Span::styled(
            format!(" {} ", follow_indicator),
            Style::default()
                .fg(if state.follow { Color::Black } else { Color::Reset })
                .bg(if state.follow { Color::Green } else { Color::Reset })
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(sev_flags, Style::default().fg(Color::White)),
        Span::raw("  |  "),
        Span::styled(processing_str, Style::default().fg(Color::Cyan)),
        Span::raw("  |  "),
        Span::styled(pos_str, Style::default().fg(pos_color)),
    ];

    // Show search match count if a search is active.
    if !state.search_matches.is_empty() {
        left_spans.push(Span::raw("  |  "));
        left_spans.push(Span::styled(
            format!("match {}/{}", state.search_cursor + 1, state.search_matches.len()),
            Style::default().fg(Color::Yellow),
        ));
    }

    // Right side: growth rate text + sparkline.
    let rate_str = if state.stats.lines_per_sec < 0.5 {
        "-- File not growing --".to_string()
    } else {
        format!("Growth rate: {:.0} L/s", state.stats.lines_per_sec)
    };
    let right_spans = vec![
        Span::styled(rate_str, Style::default().fg(Color::Cyan)),
        Span::raw(" "),
        Span::styled(sparkline, Style::default().fg(Color::Cyan)),
        Span::raw(" "),
    ];

    // Build the line with left and right segments.
    // Calculate the right-side width to add appropriate spacing.
    let right_width: usize = right_spans.iter().map(|s| s.content.chars().count()).sum();
    let left_width: usize = left_spans.iter().map(|s| s.content.chars().count()).sum();
    let total_width = area.width as usize;
    let gap = total_width.saturating_sub(left_width + right_width);

    let mut all_spans = left_spans;
    all_spans.push(Span::raw(" ".repeat(gap)));
    all_spans.extend(right_spans);

    let line = Line::from(all_spans);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_escapes_control_chars() {
        assert_eq!(sanitize_for_terminal("hello"), "hello");
        assert_eq!(sanitize_for_terminal("\x1b[31mred\x1b[0m"), "<1B>[31mred<1B>[0m");
        assert_eq!(sanitize_for_terminal("a\x00b"), "a<00>b");
        assert_eq!(sanitize_for_terminal("a\x7fb"), "a<7F>b");
    }

    #[test]
    fn sanitize_preserves_tab_and_newline() {
        assert_eq!(sanitize_for_terminal("a\tb"), "a\tb");
        assert_eq!(sanitize_for_terminal("a\nb"), "a\nb");
        assert_eq!(sanitize_for_terminal("a\rb"), "a\rb");
    }

    #[test]
    fn sanitize_preserves_unicode() {
        assert_eq!(sanitize_for_terminal("hello — world"), "hello — world");
        assert_eq!(sanitize_for_terminal("日本語"), "日本語");
    }
}
