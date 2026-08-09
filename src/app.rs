//! Application state, event mapping, and the main TUI run loop.

use std::io::stdout;
use std::io::IsTerminal;
use std::sync::Arc;
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
use tokio::sync::Mutex;

use crate::cli::Cli;
use crate::config::Config;
use crate::io::file::open_dual;
use crate::io::reader::head_reader;
use crate::io::stdin::stdin_reader;
use crate::io::tail::tail_reader_with_initial;
use crate::io::RawLine;
use crate::pipeline::index::ReadProgress;
use crate::pipeline::parser::{ParsedLine, Parser};
use crate::repl::dispatcher::OutputFormat;
use crate::theme::Theme;
use crate::app::events::{map_event, map_input_event, AppAction, InputAction};
use crate::app::state::InputMode;

pub mod events;
pub mod state;

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

    let follow = cli.follow();
    let mut state = AppState::new(config, theme, cli.files().to_vec(), follow);

    // Set up the pipeline: readers → [raw_rx] → parser → [parsed_rx] → UI.
    let (raw_tx, raw_rx) = mpsc::channel::<RawLine>(CHANNEL_CAPACITY);
    let (parsed_tx, parsed_rx) = mpsc::channel::<ParsedLine>(CHANNEL_CAPACITY);

    // Read progress tracker — shared between head reader and REPL.
    // For multiple files, we track the first file's progress (the primary).
    // TODO: per-file progress tracking.
    let file_size = cli
        .files()
        .first()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .unwrap_or(0);
    let progress = ReadProgress::new(file_size);
    state.set_progress(progress.clone());

    // Create the in-memory SQLite database and wire it into the pipeline.
    let db = crate::db::create_shared()?;
    state.db = Some(db.clone());
    let (db_tx, db_rx) = mpsc::channel::<ParsedLine>(CHANNEL_CAPACITY);
    crate::db::spawn_db_writer(db, db_rx);

    spawn_sources(&cli, raw_tx.clone(), progress.clone(), follow);
    spawn_parser(raw_rx, parsed_tx, Some(db_tx));

    if cli.should_use_repl_mode() {
        run_repl(cli, state, raw_tx, parsed_rx, progress).await
    } else {
        enter_raw_mode()?;
        let result = run_tui_loop(&mut state, parsed_rx);
        let _ = restore_terminal();
        result
    }
}

/// Run in REPL/TCP mode (non-TTY).
async fn run_repl(
    cli: Cli,
    state: AppState,
    raw_tx: mpsc::Sender<RawLine>,
    mut parsed_rx: mpsc::Receiver<ParsedLine>,
    progress: ReadProgress,
) -> Result<()> {
    let state = Arc::new(Mutex::new(state));

    // Background task: drain parsed lines into state.
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut lines_this_sec: u64 = 0;
            let mut sec_start = Instant::now();
            loop {
                let mut new_lines = 0u64;
                while let Ok(line) = parsed_rx.try_recv() {
                    let mut s = state.lock().await;
                    s.push_line(line);
                    new_lines += 1;
                }
                if new_lines > 0 {
                    lines_this_sec += new_lines;
                    // Sort by byte offset to merge head and tail output.
                    // Save anchor for scroll restoration.
                    let mut s = state.lock().await;
                    let anchor_offset = if !s.follow && !s.lines.is_empty() {
                        let idx = s.scroll.min(s.lines.len() - 1);
                        Some(s.lines[idx].byte_offset)
                    } else {
                        None
                    };
                    s.lines.sort_by_key(|l| l.byte_offset);
                    renumber_lines(&mut s);
                    if s.follow {
                        s.scroll_to_bottom();
                    } else if let Some(anchor) = anchor_offset {
                        let new_idx = s
                            .lines
                            .partition_point(|l| l.byte_offset < anchor);
                        s.scroll = new_idx.min(s.max_scroll());
                    }
                }
                if sec_start.elapsed() >= Duration::from_secs(1) {
                    let mut s = state.lock().await;
                    s.stats.lines_per_sec = lines_this_sec as f64 / sec_start.elapsed().as_secs_f64();
                    lines_this_sec = 0;
                    sec_start = Instant::now();
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
    }

    let format = if cli.json() {
        OutputFormat::Json
    } else {
        OutputFormat::Text
    };

    // Start TCP server if --listen is given.
    if let Some(addr) = cli.listen().map(str::to_owned) {
        let state = state.clone();
        let raw_tx = raw_tx.clone();
        let progress = progress.clone();
        let db = state.lock().await.db.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::repl::tcp::run(&addr, state, raw_tx, progress, db, format).await {
                tracing::error!("TCP server: {e:#}");
            }
        });
        // Give the TCP server a moment to bind.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    // Run stdin REPL if stdin is not closed (e.g. /dev/null gives EOF
    // immediately). When --listen is given and stdin is a TTY or /dev/null,
    // we wait for ctrl-c instead of exiting when stdin closes.
    let stdin_is_tty = std::io::stdin().is_terminal();
    if (cli.repl() || cli.listen().is_none()) && !stdin_is_tty {
        let repl = crate::repl::dispatcher::ReplState {
            state: state.clone(),
            raw_tx: raw_tx.clone(),
            progress: progress.clone(),
            db: state.lock().await.db.clone(),
        };
        crate::repl::stdin::run(repl, format).await?;
    }
    if cli.listen().is_some() {
        // Keep running for the TCP server until ctrl-c.
        tokio::signal::ctrl_c().await.ok();
    }

    Ok(())
}

/// Spawn readers for each file, or stdin.
///
/// Both head and tail readers always start simultaneously:
/// - Head reader: reads from byte 0 to `tail_start` (stops before the tail's
///   initial read region to avoid duplicates).
/// - Tail reader: seeks to `EOF - 64KB`, reads forward to EOF (initial
///   screenful), then follows appends.
///
/// This means HOME shows the beginning immediately, END shows the last
/// lines immediately, and both work in parallel. When the head reader
/// reaches `tail_start`, the two views merge seamlessly.
fn spawn_sources(cli: &Cli, raw_tx: mpsc::Sender<RawLine>, progress: ReadProgress, _follow: bool) {
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

    for (i, path) in cli.files().iter().enumerate() {
        let dual = match open_dual(path) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("open {}: {e:#}", path.display());
                continue;
            }
        };
        let source = path.to_string_lossy().to_string();
        let size = dual.size;
        let tail_start = dual.tail_start;
        if i == 0 {
            progress.set_tail_start(tail_start);
        }

        // Head reader: reads from byte 0 to tail_start (stops before tail's region).
        let tx_head = raw_tx.clone();
        let src_head = source.clone();
        let head_file = dual.head;
        let head_progress = if i == 0 {
            progress.clone()
        } else {
            ReadProgress::new(size)
        };
        tokio::task::spawn_blocking(move || {
            if let Err(e) = head_reader(head_file, tail_start, src_head, tx_head, head_progress) {
                tracing::error!("head reader: {e:#}");
            }
        });

        // Tail reader: initial backward read from (EOF - 64KB) to EOF,
        // then follows appends. Always runs.
        let tx_tail = raw_tx.clone();
        let src_tail = source.clone();
        let tail_file = dual.tail;
        let tail_path = path.clone();
        let tail_progress = if i == 0 {
            Some(progress.clone())
        } else {
            None
        };
        tokio::task::spawn_blocking(move || {
            if let Err(e) = tail_reader_with_initial(
                tail_file,
                size,
                tail_path,
                src_tail,
                tx_tail,
                crate::io::file::TAIL_INITIAL_READ,
                tail_progress,
            ) {
                tracing::error!("tail reader: {e:#}");
            }
        });
    }

    // Drop our own sender so the channel closes when all readers finish.
    drop(raw_tx);
}

/// Renumber lines after sorting by byte_offset.
///
/// Three cases:
/// 1. Head done: number all lines sequentially 1..N (sort gives correct order).
/// 2. Tail only (no head lines): start from estimated_total - N + 1.
/// 3. Both head and tail lines, head not done: number head lines 1..head_count,
///    tail lines from estimated_total - tail_count + 1. There's a gap between
///    them, which is fine — binary search handles it.
fn renumber_lines(state: &mut AppState) {
    if state.lines.is_empty() {
        return;
    }

    let head_done = state.progress.head_done();
    let tail_start = state.progress.tail_start();

    if head_done {
        // Head is done — number everything sequentially.
        for (i, line) in state.lines.iter_mut().enumerate() {
            line.line_no = (i + 1) as u64;
        }
        return;
    }

    // Count head and tail lines.
    let mut head_count = 0u64;
    let mut tail_count = 0u64;
    for line in &state.lines {
        if line.byte_offset < tail_start {
            head_count += 1;
        } else {
            tail_count += 1;
        }
    }

    if tail_count == 0 {
        // Only head lines — number sequentially.
        for (i, line) in state.lines.iter_mut().enumerate() {
            line.line_no = (i + 1) as u64;
        }
    } else if head_count == 0 {
        // Only tail lines — start from estimated total.
        let n = state.lines.len() as u64;
        let estimated = state.progress.estimated_total_lines();
        let start = if estimated > n {
            estimated - n + 1
        } else {
            1
        };
        for (i, line) in state.lines.iter_mut().enumerate() {
            line.line_no = start + i as u64;
        }
    } else {
        // Both head and tail lines, head not done.
        // Head lines: 1, 2, ..., head_count
        // Tail lines: estimated_total - tail_count + 1, ..., estimated_total
        let estimated = state.progress.estimated_total_lines();
        let tail_start_no = if estimated > tail_count {
            estimated - tail_count + 1
        } else {
            head_count + 1
        };
        let mut head_no = 1u64;
        let mut tail_no = tail_start_no;
        for line in state.lines.iter_mut() {
            if line.byte_offset < tail_start {
                line.line_no = head_no;
                head_no += 1;
            } else {
                line.line_no = tail_no;
                tail_no += 1;
            }
        }
    }
}

/// Spawn the parser task that transforms raw lines into parsed lines.
/// If a DB sender is provided, parsed lines are cloned and sent to the DB
/// channel as well.
fn spawn_parser(
    mut raw_rx: mpsc::Receiver<RawLine>,
    parsed_tx: mpsc::Sender<ParsedLine>,
    db_tx: Option<mpsc::Sender<ParsedLine>>,
) {
    tokio::spawn(async move {
        let mut parser = Parser::new();
        while let Some(raw) = raw_rx.recv().await {
            let parsed = parser.parse(raw);
            // Send to DB first (non-critical: if DB is slow, don't block UI).
            if let Some(ref db_tx) = db_tx {
                let _ = db_tx.try_send(parsed.clone());
            }
            if parsed_tx.send(parsed).await.is_err() {
                break; // UI dropped the receiver (app quit)
            }
        }
        tracing::debug!("parser task done");
    });
}

/// The main TUI event loop. Polls keyboard input with a short timeout and
/// drains parsed lines from the channel between polls.
fn run_tui_loop(
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
            // Save the byte_offset of the first visible line so we can
            // restore the scroll position after sorting (head reader may
            // insert lines before the current position, shifting indices).
            let anchor_offset = if !state.follow && !state.lines.is_empty() {
                let idx = state.scroll.min(state.lines.len() - 1);
                Some(state.lines[idx].byte_offset)
            } else {
                None
            };
            // Sort lines by byte offset to merge head and tail reader output.
            state.lines.sort_by_key(|l| l.byte_offset);
            // Renumber lines.
            renumber_lines(state);
            if state.follow {
                // If following, snap to the bottom.
                state.scroll_to_bottom();
            } else if let Some(anchor) = anchor_offset {
                // Restore scroll to the same line (by byte_offset).
                let new_idx = state
                    .lines
                    .partition_point(|l| l.byte_offset < anchor);
                state.scroll = new_idx.min(state.max_scroll());
            }
            // Recompute search matches if a search is active.
            if state.search.is_some() {
                state.recompute_search_matches();
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
            // Drain all pending key events before rendering — coalesces
            // rapid key presses (e.g. 4x page-down) into a single render.
            loop {
                let ev = event::read()?;
                if let Event::Key(key) = ev {
                    if key.kind != KeyEventKind::Press {
                        // Check if there are more events before breaking.
                        if !event::poll(Duration::from_millis(0))? {
                            break;
                        }
                        continue;
                    }

                    // In input mode, handle character typing directly.
                    if state.input_mode != InputMode::Normal {
                        match map_input_event(key) {
                            InputAction::Char(c) => {
                                state.input_buffer.push(c);
                            }
                            InputAction::Backspace => {
                                state.input_buffer.pop();
                            }
                            InputAction::Confirm => {
                                state.apply(AppAction::ConfirmInput);
                            }
                            InputAction::Cancel => {
                                state.apply(AppAction::CancelInput);
                            }
                            InputAction::Ignore => {}
                        }
                    } else {
                        match map_event(key) {
                            AppAction::Quit => {
                                state.quit_requested = true;
                                break;
                            }
                            AppAction::Noop => {}
                            other => state.apply(other),
                        }
                    }
                }
                // Non-blocking check: are there more events queued?
                if !event::poll(Duration::from_millis(0))? {
                    break;
                }
            }
            if state.quit_requested {
                break;
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
