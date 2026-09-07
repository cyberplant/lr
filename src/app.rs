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
use crate::io::line_index::LineIndex;
use crate::io::reader::head_reader;
use crate::io::stdin::{stdin_reader, stdin_to_temp_file, cleanup_temp_file, StdinSpillResult};
use crate::io::tail::tail_reader_with_initial;
use crate::io::RawLine;
use crate::pipeline::index::ReadProgress;
use crate::pipeline::parser::{ParsedLine, Parser};
use crate::repl::dispatcher::OutputFormat;
use crate::theme::Theme;
use crate::app::events::{map_event, map_input_event, AppAction, InputAction};
use crate::app::state::InputMode;
use crate::app::store::LineStore;

pub mod events;
pub mod state;
pub mod store;

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

    // Set up the pipeline: tail/stdin readers → [raw_rx] → parser → [parsed_rx] → UI.
    // The head reader no longer sends lines through the parser — it builds
    // a byte offset index (LineIndex) instead. Lines are read on demand
    // from the file by the LineStore.
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

    // Create a shared LineIndex for the head reader and a LineStore for
    // on-demand line access. The head reader builds the index; the LineStore
    // uses it (plus a file handle) to read lines on demand.
    let is_stdin = cli.stdin() || cli.files().is_empty();
    let line_index = Arc::new(LineIndex::new(file_size));

    // Determine stdin mode: CLI override takes priority, then config.
    let stdin_mode = cli.stdin_mode().unwrap_or(state.config.stdin.mode);
    let stdin_mem_limit_mb = cli
        .stdin_memory_limit_mb()
        .unwrap_or(state.config.stdin.memory_limit_mb);

    // For stdin temp-file mode, we need to spawn the spilling task and
    // get the temp file handle back before creating the LineStore.
    // We use a channel to receive the spill result.
    let stdin_spill_rx = if is_stdin && stdin_mode == crate::config::StdinMode::TempFile {
        let (spill_tx, spill_rx) = tokio::sync::oneshot::channel::<StdinSpillResult>();
        let spill_index = line_index.clone();
        let spill_progress = progress.clone();
        tokio::task::spawn_blocking(move || {
            match stdin_to_temp_file(spill_index, spill_progress) {
                Ok(result) => {
                    let _ = spill_tx.send(result);
                }
                Err(e) => {
                    tracing::error!("stdin temp file spill: {e:#}");
                }
            }
        });
        Some(spill_rx)
    } else {
        None
    };

    // For file mode or stdin memory mode, create the store immediately.
    // For stdin temp-file mode, the store will be set up after the spill
    // completes (or we start with a placeholder and swap it).
    let store_file = if is_stdin {
        None // temp-file mode will set it later; memory mode doesn't need it.
    } else {
        cli.files().first().and_then(|p| std::fs::File::open(p).ok())
    };
    let store_source = cli
        .files()
        .first()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "stdin".to_string());

    let mut store = LineStore::new(line_index.clone(), store_file, store_source, is_stdin);
    if is_stdin && stdin_mode == crate::config::StdinMode::Memory {
        store.set_memory_limit(stdin_mem_limit_mb * 1024 * 1024);
    }
    state.set_store(store);

    spawn_sources(&cli, raw_tx.clone(), progress.clone(), follow, line_index, is_stdin, stdin_mode);
    spawn_parser(raw_rx, parsed_tx, Some(db_tx));

    // For stdin temp-file mode, spawn a task that waits for the spill to
    // complete, then updates the store with the temp file handle. This
    // allows the TUI/REPL to start immediately while stdin is being spilled.
    if let Some(spill_rx) = stdin_spill_rx {
        // We need to pass the store update to a background task.
        // Since AppState is not Send, we can't move it into a task.
        // Instead, we use a channel to send the spill result to the
        // main loop, which polls it alongside parsed_rx.
        // For simplicity, we spawn a tokio task that awaits the spill
        // and stores the result in a shared cell that the main loop checks.
        let spill_result = Arc::new(tokio::sync::Mutex::new(None));
        let spill_result_clone = spill_result.clone();
        tokio::spawn(async move {
            if let Ok(result) = spill_rx.await {
                *spill_result_clone.lock().await = Some(result);
            }
        });
        // Store the shared cell on state so the main loop can check it.
        state.spill_result = Some(spill_result);
    }

    // Run the app.
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
                // Check if stdin temp-file spill has completed.
                {
                    let mut s = state.lock().await;
                    if s.spill_result.is_some() {
                        s.try_apply_spill_result();
                    }
                }
                let mut new_lines = 0u64;
                while let Ok(line) = parsed_rx.try_recv() {
                    let mut s = state.lock().await;
                    s.push_tail_line(line);
                    new_lines += 1;
                }
                if new_lines > 0 {
                    lines_this_sec += new_lines;
                    // Save anchors for scroll and cursor restoration.
                    let mut s = state.lock().await;
                    let (anchor_offset, cursor_offset) = if !s.follow && !s.store.is_empty() {
                        let scroll_idx = s.scroll.min(s.store.len() - 1);
                        let cursor_idx = s.cursor.min(s.store.len() - 1);
                        (s.store.offset(scroll_idx), s.store.offset(cursor_idx))
                    } else {
                        (None, None)
                    };
                    if s.follow {
                        let len = s.store.len();
                        if s.filtering_active() {
                            s.cursor = s.prev_visible_from(len.saturating_sub(1)).unwrap_or(0);
                        } else if len > 0 {
                            s.cursor = len - 1;
                        }
                        s.scroll_to_bottom();
                    } else if let Some(anchor) = anchor_offset {
                        // Restore scroll to the line at the saved byte offset.
                        let new_idx = s.store.line_at_offset(anchor);
                        s.scroll = new_idx.min(s.max_scroll());
                        if let Some(cursor_anchor) = cursor_offset {
                            s.cursor = s.store.line_at_offset(cursor_anchor);
                            s.clamp_cursor();
                        }
                    }
                }
                if sec_start.elapsed() >= Duration::from_secs(1) {
                    let mut s = state.lock().await;
                    s.stats.lines_per_sec = lines_this_sec as f64 / sec_start.elapsed().as_secs_f64();
                    let rate = s.stats.lines_per_sec;
                    s.stats.push_rate_sample(rate);
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

    // Clean up stdin temp file if one was created.
    if let Some(path) = state.lock().await.spill_cleanup_path.take() {
        cleanup_temp_file(&path);
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
fn spawn_sources(cli: &Cli, raw_tx: mpsc::Sender<RawLine>, progress: ReadProgress, _follow: bool, line_index: Arc<LineIndex>, _is_stdin: bool, stdin_mode: crate::config::StdinMode) {
    if cli.files().is_empty() && !cli.stdin() {
        return;
    }

    // Stdin handling: in temp-file mode, the spill task is already spawned
    // in run(). In memory mode, spawn the line-by-line reader.
    if cli.stdin() && stdin_mode == crate::config::StdinMode::Memory {
        let tx = raw_tx.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = stdin_reader(tx) {
                tracing::error!("stdin reader: {e:#}");
            }
        });
    }

    // For file mode, spawn head + tail readers as before.
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

        // Head reader: scans from byte 0 to tail_start, building the byte
        // offset index. No longer sends lines through the parser pipeline.
        let head_file = dual.head;
        let head_progress = if i == 0 {
            progress.clone()
        } else {
            ReadProgress::new(size)
        };
        // Only the first file shares the primary LineIndex; subsequent
        // files get their own index (TODO: multi-file store support).
        let head_index = if i == 0 {
            line_index.clone()
        } else {
            Arc::new(LineIndex::new(size))
        };
        tokio::task::spawn_blocking(move || {
            if let Err(e) = head_reader(head_file, tail_start, head_index, head_progress) {
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
    let mut spill_cleanup_path: Option<std::path::PathBuf> = None;

    loop {
        // Check if stdin temp-file spill has completed.
        if let Some(path) = state.try_apply_spill_result() {
            spill_cleanup_path = Some(path);
        }
        // Drain all available parsed lines into state.
        let mut new_lines = 0u64;
        while let Ok(line) = parsed_rx.try_recv() {
            state.push_tail_line(line);
            new_lines += 1;
        }
        if new_lines > 0 {
            lines_this_sec += new_lines;
            // Save the byte_offset of the first visible line and the cursor
            // line so we can restore their positions after sorting (head
            // reader may insert lines before the current position, shifting
            // indices).
            let (anchor_offset, cursor_offset) = if !state.follow && !state.store.is_empty() {
                let scroll_idx = state.scroll.min(state.store.len() - 1);
                let cursor_idx = state.cursor.min(state.store.len() - 1);
                (state.store.offset(scroll_idx), state.store.offset(cursor_idx))
            } else {
                (None, None)
            };
            if state.follow {
                // If following, snap cursor and scroll to the bottom.
                // When filtering is active, snap to the last visible line
                // (not the last raw line, which may be hidden).
                let len = state.store.len();
                if state.filtering_active() {
                    state.cursor = state.prev_visible_from(len.saturating_sub(1)).unwrap_or(0);
                } else if len > 0 {
                    state.cursor = len - 1;
                }
                state.scroll_to_bottom();
            } else if let Some(anchor) = anchor_offset {
                // Restore scroll to the same line (by byte_offset).
                let new_idx = state.store.line_at_offset(anchor);
                state.scroll = new_idx.min(state.max_scroll());
                // Restore cursor to the same line (by byte_offset).
                if let Some(cursor_anchor) = cursor_offset {
                    state.cursor = state.store.line_at_offset(cursor_anchor);
                    // Clamp cursor in case of edge cases.
                    state.clamp_cursor();
                }
            }
            // Recompute search matches if a search is active.
            if state.search.is_some() {
                state.recompute_search_matches();
            }
        }

        // Update lines/sec counter every second.
        if sec_start.elapsed() >= Duration::from_secs(1) {
            state.stats.lines_per_sec = lines_this_sec as f64 / sec_start.elapsed().as_secs_f64();
            state.stats.push_rate_sample(state.stats.lines_per_sec);
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
                            AppAction::Refresh => {
                                terminal.clear()?;
                                terminal.draw(|frame| crate::ui::render(frame, state))?;
                                last_render = Instant::now();
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
    // Clean up stdin temp file if one was created.
    if let Some(path) = spill_cleanup_path {
        cleanup_temp_file(&path);
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
