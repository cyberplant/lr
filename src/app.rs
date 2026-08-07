//! Application state, event mapping, and the main TUI run loop.

use std::io::stdout;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::cli::Cli;
use crate::config::Config;
use crate::io::file::open_dual;
use crate::io::reader::head_reader;
use crate::io::stdin::stdin_reader;
use crate::io::tail::tail_reader;
use crate::io::RawLine;
use crate::pipeline::parser::{ParsedLine, Parser};
use crate::theme::Theme;

pub mod events;
pub mod state;

use events::{map_event, AppAction};
use state::AppState;

/// Channel capacity for raw lines (reader → parser) and parsed lines
/// (parser → UI). Large enough to absorb bursts; backpressure naturally
/// slows readers when the UI can't keep up.
const CHANNEL_CAPACITY: usize = 10_000;

/// Entry point invoked by `main`. Sets up the terminal, spawns background
/// tasks, runs the event loop, and guarantees terminal restoration on exit.
pub async fn run(cli: Cli) -> Result<()> {
    let config = Config::load(cli.config_path().map(std::path::Path::new))?;
    let theme = Theme::default_theme();
    if cli.print_config() {
        print_effective_config(&config, &theme);
        return Ok(());
    }

    let mut state = AppState::new(config, theme, cli.files().to_vec());

    // Set up the pipeline: readers → [raw_rx] → parser → [parsed_rx] → UI.
    let (raw_tx, raw_rx) = mpsc::channel::<RawLine>(CHANNEL_CAPACITY);
    let (parsed_tx, parsed_rx) = mpsc::channel::<ParsedLine>(CHANNEL_CAPACITY);

    spawn_sources(&cli, raw_tx);
    spawn_parser(raw_rx, parsed_tx);

    enter_raw_mode()?;
    let result = run_loop(&mut state, parsed_rx);
    let _ = restore_terminal();
    result
}

/// Spawn head and tail readers for each file, or a stdin reader.
fn spawn_sources(cli: &Cli, raw_tx: mpsc::Sender<RawLine>) {
    if cli.files().is_empty() && !cli.stdin() {
        return;
    }

    if cli.stdin() {
        let tx = raw_tx.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = stdin_reader(tx) {
                tracing::error!("stdin reader: {e:#}");
            }
        });
    }

    for path in cli.files() {
        let dual = match open_dual(path) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("open {}: {e:#}", path.display());
                continue;
            }
        };
        let source = path.to_string_lossy().to_string();
        let size = dual.size;

        // Head reader: reads from byte 0 to initial EOF.
        let tx_head = raw_tx.clone();
        let src_head = source.clone();
        let head_file = dual.head;
        tokio::task::spawn_blocking(move || {
            if let Err(e) = head_reader(head_file, size, src_head, tx_head) {
                tracing::error!("head reader: {e:#}");
            }
        });

        // Tail reader: follows appends from initial EOF onward.
        let tx_tail = raw_tx.clone();
        let src_tail = source.clone();
        let tail_file = dual.tail;
        let tail_path = path.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = tail_reader(tail_file, size, tail_path, src_tail, tx_tail) {
                tracing::error!("tail reader: {e:#}");
            }
        });
    }

    // Drop our own sender so the channel closes when all readers finish.
    // (The tail reader holds a clone and runs indefinitely, so the channel
    // only closes if there are no files and no stdin.)
    drop(raw_tx);
}

/// Spawn the parser task that transforms raw lines into parsed lines.
fn spawn_parser(mut raw_rx: mpsc::Receiver<RawLine>, parsed_tx: mpsc::Sender<ParsedLine>) {
    tokio::spawn(async move {
        let mut parser = Parser::new();
        while let Some(raw) = raw_rx.recv().await {
            let parsed = parser.parse(raw);
            if parsed_tx.send(parsed).await.is_err() {
                break; // UI dropped the receiver (app quit)
            }
        }
        tracing::debug!("parser task done");
    });
}

/// The main UI event loop. Polls keyboard input with a short timeout and
/// drains parsed lines from the channel between polls.
fn run_loop(
    state: &mut AppState,
    parsed_rx: mpsc::Receiver<ParsedLine>,
) -> Result<()> {
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    let mut parsed_rx = parsed_rx;
    let mut last_render = Instant::now();
    let mut lines_this_sec: u64 = 0;
    let mut sec_start = Instant::now();

    loop {
        // Drain all available parsed lines into state.
        let mut new_lines = 0u64;
        while let Ok(line) = parsed_rx.try_recv() {
            state.push_line(line);
            new_lines += 1;
        }
        if new_lines > 0 {
            lines_this_sec += new_lines;
            // If following, snap to the bottom.
            if state.follow {
                state.scroll_to_bottom();
            }
        }

        // Update lines/sec counter every second.
        if sec_start.elapsed() >= Duration::from_secs(1) {
            state.stats.lines_per_sec = lines_this_sec as f64 / sec_start.elapsed().as_secs_f64();
            lines_this_sec = 0;
            sec_start = Instant::now();
        }

        // Render at most ~30fps to avoid burning CPU.
        if last_render.elapsed() >= Duration::from_millis(33) {
            terminal.draw(|frame| crate::ui::render(frame, state))?;
            last_render = Instant::now();
        }

        // Poll for input with a short timeout.
        if event::poll(Duration::from_millis(50))? {
            let ev = event::read()?;
            if let Event::Key(key) = ev {
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
