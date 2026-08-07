//! Application state, event mapping, and the main TUI run loop.

use std::io::stdout;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::cli::Cli;
use crate::config::Config;
use crate::theme::Theme;

pub mod events;
pub mod state;

use events::{map_event, AppAction};
use state::AppState;

/// Entry point invoked by `main`. Sets up the terminal, runs the event loop,
/// and guarantees terminal restoration on exit.
pub fn run(cli: Cli) -> Result<()> {
    let config = Config::load(cli.config_path().map(std::path::Path::new))?;
    let theme = Theme::default_theme();
    if cli.print_config() {
        print_effective_config(&config, &theme);
        return Ok(());
    }

    // TODO(phase 1): open files via `io::file::open_dual`, spawn head/tail tasks.
    // TODO(phase 2): build plugin registry and parser pipeline.
    // For now we just exercise the TUI scaffolding with an empty state.
    let mut state = AppState::new(config, theme, cli.files().to_vec());

    enter_raw_mode()?;
    let result = run_loop(&mut state);
    // Always restore the terminal, even on error.
    let _ = restore_terminal();
    result
}

fn run_loop(state: &mut AppState) -> Result<()> {
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    loop {
        terminal.draw(|frame| crate::ui::render(frame, state))?;

        // Poll with a short timeout so background tasks can push updates into
        // state via channels (wired up in later phases).
        if event::poll(Duration::from_millis(100))? {
            let ev = event::read()?;
            if let Event::Key(key) = ev {
                // Only react to press events (not repeat/release) on platforms
                // that report them.
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match map_event(key) {
                    AppAction::Quit => break,
                    AppAction::Noop => {}
                    other => state.apply(other),
                }
            }
        }
    }
    Ok(())
}

fn enter_raw_mode() -> Result<()> {
    stdout()
        .execute(EnterAlternateScreen)
        .context("enter alternate screen")?;
    enable_raw_mode().context("enable raw mode")?;
    Ok(())
}

fn restore_terminal() -> Result<()> {
    disable_raw_mode().context("disable raw mode")?;
    stdout()
        .execute(LeaveAlternateScreen)
        .context("leave alternate screen")?;
    Ok(())
}

fn print_effective_config(config: &Config, theme: &Theme) {
    println!(
        "{}",
        toml::to_string_pretty(config).unwrap_or_else(|e| format!("<serialize error: {e}>"))
    );
    println!(
        "# theme (not part of config.toml): {:?}",
        theme.severity.error
    );
}

#[cfg(test)]
mod tests {
    // We don't actually toggle raw mode in a unit test (no tty), but the
    // function should not panic when the terminal is already restored.
    // Skipped in CI without a tty; just ensure it compiles.
}
