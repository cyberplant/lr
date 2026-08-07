//! TUI rendering. Phase 0 draws a placeholder layout; later phases add the
//! log view, status bar, command bar, sidebar, and help overlay.

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::state::AppState;

pub fn render(frame: &mut Frame, state: &AppState) {
    let area = frame.area();

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

    let title = if state.files.is_empty() {
        "lr — (no file)".to_string()
    } else {
        format!("lr — {}", state.files.first().unwrap().display())
    };

    let view = Paragraph::new(vec![
        Line::from(Span::styled(
            "LR — Log Reader",
            Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan),
        )),
        Line::from(""),
        Line::from(state.message.clone()),
        Line::from(""),
        Line::from("press ? for help, q to quit"),
    ])
    .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(view, main[0]);

    let status = Paragraph::new(Line::from(vec![
        Span::raw("files: "),
        Span::styled(
            state.files.len().to_string(),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw("  |  buffer: 0 lines  |  phase 0 skeleton"),
    ]))
    .style(Style::default().bg(Color::DarkGray));
    frame.render_widget(status, chunks[1]);

    let cmd = Paragraph::new(Line::from(Span::styled(
        " — press : for command, / to search, q to quit",
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(cmd, chunks[2]);
}
