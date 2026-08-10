//! Command dispatcher: executes a `Command` against `AppState` and returns
//! a text or JSON response.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::sync::Mutex;

use crate::app::events::AppAction;
use crate::app::state::AppState;
use crate::io::file::open_dual;
use crate::io::reader::head_reader;
use crate::io::tail::tail_reader_with_initial;
use crate::io::RawLine;
use crate::pipeline::index::ReadProgress;
use crate::plugin::Severity;
use crate::repl::command::{Command, ReadFileMode};

/// Output format for command responses.
#[derive(Debug, Clone, Copy)]
pub enum OutputFormat {
    Text,
    Json,
}

/// Result of dispatching a command.
#[derive(Debug)]
pub struct DispatchResult {
    /// Whether to exit the REPL loop.
    pub quit: bool,
    /// The response text to print.
    pub output: String,
}

impl DispatchResult {
    fn ok(output: String) -> Self {
        Self { quit: false, output }
    }
    fn quit(output: String) -> Self {
        Self { quit: true, output }
    }
}

/// Shared state for the REPL: the app state behind a mutex, the raw line
/// sender so the `open` command can spawn new readers, and read progress
/// for the `readfile` command.
pub struct ReplState {
    pub state: Arc<Mutex<AppState>>,
    pub raw_tx: mpsc::Sender<RawLine>,
    pub progress: ReadProgress,
    pub db: Option<crate::db::SharedDb>,
}

/// Execute a command against the shared state.
pub async fn dispatch(
    cmd: Command,
    repl: &ReplState,
    format: OutputFormat,
) -> DispatchResult {
    match format {
        OutputFormat::Text => dispatch_text(cmd, repl).await,
        OutputFormat::Json => dispatch_json(cmd, repl).await,
    }
}

// ── Text output ──────────────────────────────────────────────────────────

async fn dispatch_text(cmd: Command, repl: &ReplState) -> DispatchResult {
    match cmd {
        Command::Empty => DispatchResult::ok(String::new()),
        Command::Quit => DispatchResult::quit("bye\n".into()),
        Command::Help => DispatchResult::ok(help_text()),
        Command::Unknown { raw } => DispatchResult::ok(format!("error: unknown command: {raw}\n")),
        Command::Open { path } => {
            let path = PathBuf::from(&path);
            match open_dual(&path) {
                Ok(dual) => {
                    let source = path.to_string_lossy().to_string();
                    let size = dual.size;
                    let tail_start = dual.tail_start;
                    // Update the progress tracker for this new file.
                    repl.progress.update_file_size(size);
                    repl.progress.set_tail_start(tail_start);
                    let tx = repl.raw_tx.clone();
                    let src = source.clone();
                    let head_file = dual.head;
                    let p = repl.progress.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Err(e) = head_reader(head_file, tail_start, src, tx, p) {
                            tracing::error!("head reader: {e:#}");
                        }
                    });
                    let tx = repl.raw_tx.clone();
                    let src = source.clone();
                    let tail_file = dual.tail;
                    let tail_path = path.clone();
                    let p = repl.progress.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Err(e) = tail_reader_with_initial(
                            tail_file,
                            size,
                            tail_path,
                            src,
                            tx,
                            crate::io::file::TAIL_INITIAL_READ,
                            Some(p),
                        ) {
                            tracing::error!("tail reader: {e:#}");
                        }
                    });
                    DispatchResult::ok(format!("opened {} ({} bytes)\n", path.display(), size))
                }
                Err(e) => DispatchResult::ok(format!("error: open {}: {e:#}\n", path.display())),
            }
        }
        Command::Show { plain } => {
            let state = repl.state.lock().await;
            DispatchResult::ok(render_viewport(&state, plain))
        }
        Command::Goto { line } => {
            let mut state = repl.state.lock().await;
            // Convert absolute line number to vector index using binary
            // search (line numbers may have a gap between head and tail).
            let target = line_to_index(&state, line);
            state.cursor = target;
            state.scroll = target.min(state.max_scroll());
            state.follow = false;
            DispatchResult::ok(format!("at line {}\n", line))
        }
        Command::Page { n } => {
            let mut state = repl.state.lock().await;
            let vh = state.visible_height();
            let target = (n as usize * vh).min(state.max_scroll());
            state.cursor = target;
            state.scroll = target;
            state.follow = false;
            DispatchResult::ok(format!("at page {}\n", n))
        }
        Command::Home => {
            let mut state = repl.state.lock().await;
            state.apply(AppAction::Home);
            DispatchResult::ok("at top\n".into())
        }
        Command::End => {
            let mut state = repl.state.lock().await;
            state.apply(AppAction::End);
            let last_line = state.lines.last().map(|l| l.line_no).unwrap_or(0);
            DispatchResult::ok(format!("at end (line {})\n", last_line))
        }
        Command::Follow { on } => {
            let mut state = repl.state.lock().await;
            state.follow = on;
            if on {
                state.scroll_to_bottom();
            }
            DispatchResult::ok(format!("follow {}\n", if on { "on" } else { "off" }))
        }
        Command::Severity { level, on } => {
            let mut state = repl.state.lock().await;
            let action = match level.to_ascii_uppercase() {
                'E' => AppAction::ToggleSeverityError,
                'W' => AppAction::ToggleSeverityWarn,
                'I' => AppAction::ToggleSeverityInfo,
                'D' => AppAction::ToggleSeverityDebug,
                'T' => AppAction::ToggleSeverityTrace,
                _ => return DispatchResult::ok(format!("error: unknown severity '{level}'\n")),
            };
            // If the current state already matches the target, do nothing.
            // Otherwise toggle to flip it.
            let current = match action {
                AppAction::ToggleSeverityError => state.severity_visible.error,
                AppAction::ToggleSeverityWarn => state.severity_visible.warn,
                AppAction::ToggleSeverityInfo => state.severity_visible.info,
                AppAction::ToggleSeverityDebug => state.severity_visible.debug,
                AppAction::ToggleSeverityTrace => state.severity_visible.trace,
                _ => false,
            };
            if current != on {
                state.apply(action);
            }
            DispatchResult::ok(format!("severity {} {}\n", level, if on { "on" } else { "off" }))
        }
        Command::Search { pattern } => {
            let mut state = repl.state.lock().await;
            state.message = format!("search: {pattern}");
            DispatchResult::ok(format!("search set: {pattern}\n"))
        }
        Command::Filter { expr } => {
            let mut state = repl.state.lock().await;
            state.message = format!("filter: {expr}");
            DispatchResult::ok(format!("filter set: {expr}\n"))
        }
        Command::Sql { query } => {
            match &repl.db {
                Some(db) => {
                    let db = db.clone();
                    match tokio::task::spawn_blocking(move || {
                        let store = db.blocking_lock();
                        store.query(&query)
                    }).await {
                        Ok(Ok(rows)) => {
                            if rows.is_empty() {
                                DispatchResult::ok("(no rows)\n".into())
                            } else {
                                let mut out = format_query_result(&rows);
                                out.push('\n');
                                DispatchResult::ok(out)
                            }
                        }
                        Ok(Err(e)) => DispatchResult::ok(format!("sql error: {e}\n")),
                        Err(e) => DispatchResult::ok(format!("sql error: task join: {e}\n")),
                    }
                }
                None => DispatchResult::ok("sql: database not available\n".into()),
            }
        }
        Command::Histogram { bucket_secs } => {
            match &repl.db {
                Some(db) => {
                    let db = db.clone();
                    match tokio::task::spawn_blocking(move || {
                        let store = db.blocking_lock();
                        store.histogram(bucket_secs)
                    }).await {
                        Ok(Ok(buckets)) => {
                            if buckets.is_empty() {
                                DispatchResult::ok("(no timestamps found)\n".into())
                            } else {
                                let mut out = String::new();
                                out.push_str(&format!("histogram ({}s buckets):\n", bucket_secs));
                                let max_count = buckets.iter().map(|(_, c)| *c).max().unwrap_or(1);
                                for (bucket_ns, count) in &buckets {
                                    let bar_len = (*count as f64 / max_count as f64 * 40.0) as usize;
                                    let bar = "#".repeat(bar_len);
                                    let secs = bucket_ns / 1_000_000_000;
                                    out.push_str(&format!("  {:>12} | {:>6} {}\n", secs, count, bar));
                                }
                                out.push('\n');
                                DispatchResult::ok(out)
                            }
                        }
                        Ok(Err(e)) => DispatchResult::ok(format!("histogram error: {e}\n")),
                        Err(e) => DispatchResult::ok(format!("histogram error: task join: {e}\n")),
                    }
                }
                None => DispatchResult::ok("histogram: database not available\n".into()),
            }
        }
        Command::Stats => {
            let state = repl.state.lock().await;
            let total = if state.progress.lines_estimated() {
                state.progress.estimated_total_lines()
            } else {
                state.lines.len() as u64
            };
            let current_line = if state.lines.is_empty() {
                0
            } else {
                let idx = state.cursor.min(state.lines.len() - 1);
                state.lines[idx].line_no
            };
            DispatchResult::ok(format!(
                "lines: {}/{}\nlines/s: {:.1}\nfollow: {}\ncursor: {}\nscroll: {}\n",
                current_line,
                total,
                state.stats.lines_per_sec,
                state.follow,
                state.cursor,
                state.scroll,
            ))
        }
        Command::Lines { from, count } => {
            let state = repl.state.lock().await;
            DispatchResult::ok(render_lines(&state, from, count, false))
        }
        Command::Fields { line } => {
            let state = repl.state.lock().await;
            render_fields(&state, line)
        }
        Command::Json { line } => {
            let state = repl.state.lock().await;
            render_json(&state, line)
        }
        Command::ReadFile { mode } => {
            readfile(mode, repl).await
        }
    }
}

// ── JSON output ──────────────────────────────────────────────────────────

async fn dispatch_json(cmd: Command, repl: &ReplState) -> DispatchResult {
    match cmd {
        Command::Empty => DispatchResult::ok(String::new()),
        Command::Quit => DispatchResult::quit(r#"{"ok":true,"quit":true}"#.into()),
        Command::Help => DispatchResult::ok(
            serde_json::json!({
                "ok": true,
                "commands": ["open", "show", "goto", "page", "home", "end", "follow", "severity", "search", "filter", "sql", "stats", "lines", "fields", "json", "help", "quit"]
            })
            .to_string(),
        ),
        Command::Unknown { raw } => DispatchResult::ok(
            serde_json::json!({"ok": false, "error": raw}).to_string(),
        ),
        Command::Open { path } => {
            let p_path = PathBuf::from(&path);
            match open_dual(&p_path) {
                Ok(dual) => {
                    let source = p_path.to_string_lossy().to_string();
                    let size = dual.size;
                    let tail_start = dual.tail_start;
                    repl.progress.update_file_size(size);
                    repl.progress.set_tail_start(tail_start);
                    let tx = repl.raw_tx.clone();
                    let src = source.clone();
                    let head_file = dual.head;
                    let p = repl.progress.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Err(e) = head_reader(head_file, tail_start, src, tx, p) {
                            tracing::error!("head reader: {e:#}");
                        }
                    });
                    let tx = repl.raw_tx.clone();
                    let src = source.clone();
                    let tail_file = dual.tail;
                    let tail_path = p_path.clone();
                    let p = repl.progress.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Err(e) = tail_reader_with_initial(
                            tail_file,
                            size,
                            tail_path,
                            src,
                            tx,
                            crate::io::file::TAIL_INITIAL_READ,
                            Some(p),
                        ) {
                            tracing::error!("tail reader: {e:#}");
                        }
                    });
                    DispatchResult::ok(
                        serde_json::json!({"ok": true, "path": path, "size": size}).to_string(),
                    )
                }
                Err(e) => DispatchResult::ok(
                    serde_json::json!({"ok": false, "error": format!("{e:#}")}).to_string(),
                ),
            }
        }
        Command::Show { plain: _ } => {
            let state = repl.state.lock().await;
            let lines: Vec<serde_json::Value> = visible_lines(&state)
                .into_iter()
                .map(|(no, sev, raw)| {
                    serde_json::json!({
                        "no": no,
                        "sev": sev,
                        "raw": raw,
                    })
                })
                .collect();
            DispatchResult::ok(
                serde_json::json!({
                    "ok": true,
                    "scroll": state.scroll,
                    "total": state.lines.len(),
                    "follow": state.follow,
                    "lines": lines,
                })
                .to_string(),
            )
        }
        Command::Stats => {
            let state = repl.state.lock().await;
            let total = if state.progress.lines_estimated() {
                state.progress.estimated_total_lines()
            } else {
                state.lines.len() as u64
            };
            let current_line = if state.lines.is_empty() {
                0
            } else {
                let idx = state.cursor.min(state.lines.len() - 1);
                state.lines[idx].line_no
            };
            DispatchResult::ok(
                serde_json::json!({
                    "ok": true,
                    "current_line": current_line,
                    "total_lines": total,
                    "lines_per_sec": state.stats.lines_per_sec,
                    "follow": state.follow,
                    "cursor": state.cursor,
                    "scroll": state.scroll,
                })
                .to_string(),
            )
        }
        Command::Goto { line } => {
            let mut state = repl.state.lock().await;
            let target = line_to_index(&state, line);
            state.cursor = target;
            state.scroll = target.min(state.max_scroll());
            state.follow = false;
            DispatchResult::ok(r#"{"ok":true}"#.into())
        }
        Command::Page { n } => {
            let mut state = repl.state.lock().await;
            let vh = state.visible_height();
            let target = (n as usize * vh).min(state.max_scroll());
            state.cursor = target;
            state.scroll = target;
            state.follow = false;
            DispatchResult::ok(r#"{"ok":true}"#.into())
        }
        Command::Home => {
            let mut state = repl.state.lock().await;
            state.apply(AppAction::Home);
            DispatchResult::ok(r#"{"ok":true}"#.into())
        }
        Command::End => {
            let mut state = repl.state.lock().await;
            state.apply(AppAction::End);
            DispatchResult::ok(r#"{"ok":true}"#.into())
        }
        Command::Follow { on } => {
            let mut state = repl.state.lock().await;
            state.follow = on;
            if on {
                state.scroll_to_bottom();
            }
            DispatchResult::ok(r#"{"ok":true,"follow":on}"#.into())
        }
        Command::Severity { level, on } => {
            let mut state = repl.state.lock().await;
            let action = match level.to_ascii_uppercase() {
                'E' => AppAction::ToggleSeverityError,
                'W' => AppAction::ToggleSeverityWarn,
                'I' => AppAction::ToggleSeverityInfo,
                'D' => AppAction::ToggleSeverityDebug,
                'T' => AppAction::ToggleSeverityTrace,
                _ => {
                    return DispatchResult::ok(
                        serde_json::json!({"ok": false, "error": format!("unknown severity '{level}'")}).to_string(),
                    )
                }
            };
            let current = match action {
                AppAction::ToggleSeverityError => state.severity_visible.error,
                AppAction::ToggleSeverityWarn => state.severity_visible.warn,
                AppAction::ToggleSeverityInfo => state.severity_visible.info,
                AppAction::ToggleSeverityDebug => state.severity_visible.debug,
                AppAction::ToggleSeverityTrace => state.severity_visible.trace,
                _ => false,
            };
            if current != on {
                state.apply(action);
            }
            DispatchResult::ok(r#"{"ok":true}"#.into())
        }
        Command::Lines { from, count } => {
            let state = repl.state.lock().await;
            let lines: Vec<serde_json::Value> = (from..from.saturating_add(count))
                .filter_map(|i| {
                    let idx = (i as usize).saturating_sub(1);
                    let pl = state.lines.get(idx)?;
                    Some(serde_json::json!({
                        "no": pl.line_no,
                        "sev": severity_char(pl.severity),
                        "raw": pl.raw,
                    }))
                })
                .collect();
            DispatchResult::ok(serde_json::json!({"ok": true, "lines": lines}).to_string())
        }
        Command::Fields { line } => {
            let state = repl.state.lock().await;
            let idx = line_to_index(&state, line);
            match state.lines.get(idx) {
                Some(pl) => {
                    let fields: serde_json::Value = pl
                        .fields
                        .iter()
                        .map(|(k, v)| (k.clone(), field_value_to_json(v)))
                        .collect();
                    DispatchResult::ok(
                        serde_json::json!({"ok": true, "line": line, "fields": fields}).to_string(),
                    )
                }
                None => DispatchResult::ok(
                    serde_json::json!({"ok": false, "error": format!("line {line} not found")})
                        .to_string(),
                ),
            }
        }
        Command::Json { line } => {
            let state = repl.state.lock().await;
            let idx = line_to_index(&state, line);
            match state.lines.get(idx).and_then(|pl| pl.json.as_ref()) {
                Some(v) => DispatchResult::ok(
                    serde_json::json!({"ok": true, "line": line, "json": v}).to_string(),
                ),
                None => DispatchResult::ok(
                    serde_json::json!({"ok": false, "error": format!("line {line} has no JSON")})
                        .to_string(),
                ),
            }
        }
        Command::Search { pattern } => {
            let mut state = repl.state.lock().await;
            state.message = format!("search: {pattern}");
            DispatchResult::ok(r#"{"ok":true}"#.into())
        }
        Command::Filter { expr } => {
            let mut state = repl.state.lock().await;
            state.message = format!("filter: {expr}");
            DispatchResult::ok(r#"{"ok":true}"#.into())
        }
        Command::Sql { query } => {
            match &repl.db {
                Some(db) => {
                    let db = db.clone();
                    match tokio::task::spawn_blocking(move || {
                        let store = db.blocking_lock();
                        store.query(&query)
                    }).await {
                        Ok(Ok(rows)) => {
                            let rows_json: Vec<serde_json::Value> = rows.iter().map(|r| {
                                let mut obj = serde_json::Map::new();
                                for (col, val) in r.columns.iter().zip(&r.values) {
                                    obj.insert(col.clone(), serde_json::Value::String(val.clone()));
                                }
                                serde_json::Value::Object(obj)
                            }).collect();
                            DispatchResult::ok(
                                serde_json::json!({"ok": true, "rows": rows_json, "count": rows.len()})
                                    .to_string(),
                            )
                        }
                        Ok(Err(e)) => DispatchResult::ok(
                            serde_json::json!({"ok": false, "error": e.to_string()}).to_string(),
                        ),
                        Err(e) => DispatchResult::ok(
                            serde_json::json!({"ok": false, "error": format!("task join: {e}")}).to_string(),
                        ),
                    }
                }
                None => DispatchResult::ok(
                    serde_json::json!({"ok": false, "error": "database not available"}).to_string(),
                ),
            }
        }
        Command::Histogram { bucket_secs } => {
            match &repl.db {
                Some(db) => {
                    let db = db.clone();
                    match tokio::task::spawn_blocking(move || {
                        let store = db.blocking_lock();
                        store.histogram(bucket_secs)
                    }).await {
                        Ok(Ok(buckets)) => {
                            let buckets_json: Vec<serde_json::Value> = buckets.iter().map(|(b, c)| {
                                serde_json::json!({"bucket": b, "count": c})
                            }).collect();
                            DispatchResult::ok(
                                serde_json::json!({"ok": true, "buckets": buckets_json, "bucket_secs": bucket_secs})
                                    .to_string(),
                            )
                        }
                        Ok(Err(e)) => DispatchResult::ok(
                            serde_json::json!({"ok": false, "error": e.to_string()}).to_string(),
                        ),
                        Err(e) => DispatchResult::ok(
                            serde_json::json!({"ok": false, "error": format!("task join: {e}")}).to_string(),
                        ),
                    }
                }
                None => DispatchResult::ok(
                    serde_json::json!({"ok": false, "error": "database not available"}).to_string(),
                ),
            }
        }
        Command::ReadFile { mode } => {
            readfile_json(mode, repl).await
        }
    }
}

// ── readfile implementation ──────────────────────────────────────────────

/// Poll the read progress until the condition is met, with a timeout.
/// Returns a text response with progress info.
async fn readfile(mode: ReadFileMode, repl: &ReplState) -> DispatchResult {
    let progress = repl.progress.clone();
    let timeout = std::time::Duration::from_secs(30);
    let start = std::time::Instant::now();

    let result = tokio::time::timeout(timeout, async {
        loop {
            let p = &progress;
            match &mode {
                ReadFileMode::Full => {
                    if p.head_done() {
                        return;
                    }
                }
                ReadFileMode::Quick => {
                    // Quick = tail reader has completed its initial backward
                    // read (estimated_total_lines is set) AND those lines
                    // have been drained into state.
                    let state = repl.state.lock().await;
                    let has_tail_lines = state.lines.iter().any(|l| {
                        l.byte_offset >= progress.tail_start()
                    });
                    if (has_tail_lines && progress.estimated_total_lines() > 0)
                        || p.head_done()
                    {
                        return;
                    }
                }
                ReadFileMode::Percent(target) => {
                    if p.reached_fraction(*target) || p.head_done() {
                        return;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;

    let elapsed = start.elapsed();
    let p = &progress;
    let pct = p.fraction() * 100.0;
    let lines = p.line_count();
    let bytes = p.bytes_read();
    let size = p.file_size();

    match result {
        Ok(()) => {
            // Give the background drain task a moment to process the lines
            // that the head reader just finished sending through the pipeline.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let label = match &mode {
                ReadFileMode::Full => "full",
                ReadFileMode::Quick => "quick",
                ReadFileMode::Percent(t) => &format!("{:.0}%", t * 100.0),
            };
            DispatchResult::ok(format!(
                "readfile {label}: done in {elapsed:.0?} | {bytes}/{size} bytes ({pct:.1}%) | {lines} lines\n"
            ))
        }
        Err(_) => DispatchResult::ok(format!(
            "readfile: timed out after {timeout:.0?} | {bytes}/{size} bytes ({pct:.1}%) | {lines} lines\n"
        )),
    }
}

/// JSON variant of readfile.
async fn readfile_json(mode: ReadFileMode, repl: &ReplState) -> DispatchResult {
    let progress = repl.progress.clone();
    let timeout = std::time::Duration::from_secs(30);

    let result = tokio::time::timeout(timeout, async {
        loop {
            let p = &progress;
            match &mode {
                ReadFileMode::Full => {
                    if p.head_done() {
                        return;
                    }
                }
                ReadFileMode::Quick => {
                    let state = repl.state.lock().await;
                    let has_tail_lines = state.lines.iter().any(|l| {
                        l.byte_offset >= progress.tail_start()
                    });
                    if (has_tail_lines && progress.estimated_total_lines() > 0)
                        || p.head_done()
                    {
                        return;
                    }
                }
                ReadFileMode::Percent(target) => {
                    if p.reached_fraction(*target) || p.head_done() {
                        return;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;

    // Give the background drain task a moment to process lines.
    if result.is_ok() {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let p = &progress;
    DispatchResult::ok(
        serde_json::json!({
            "ok": result.is_ok(),
            "mode": match &mode {
                ReadFileMode::Full => serde_json::Value::from("full"),
                ReadFileMode::Quick => serde_json::Value::from("quick"),
                ReadFileMode::Percent(t) => serde_json::Value::from(*t),
            },
            "bytes_read": p.bytes_read(),
            "file_size": p.file_size(),
            "fraction": p.fraction(),
            "lines": p.line_count(),
            "head_done": p.head_done(),
        })
        .to_string(),
    )
}

// ── Rendering helpers ────────────────────────────────────────────────────

/// Format query results as a simple aligned table.
fn format_query_result(rows: &[crate::db::QueryRow]) -> String {
    if rows.is_empty() {
        return String::new();
    }

    // Column widths: start with header name length.
    let mut widths: Vec<usize> = rows[0]
        .columns
        .iter()
        .map(|c| c.len())
        .collect();

    for row in rows {
        for (i, val) in row.values.iter().enumerate() {
            if i < widths.len() && val.len() > widths[i] {
                widths[i] = val.len();
            }
        }
    }

    let mut out = String::new();

    // Header.
    for (i, col) in rows[0].columns.iter().enumerate() {
        if i > 0 {
            out.push_str(" | ");
        }
        out.push_str(&format!("{:<w$}", col, w = widths[i]));
    }
    out.push('\n');

    // Separator.
    for (i, w) in widths.iter().enumerate() {
        if i > 0 {
            out.push_str("-+-");
        }
        out.push_str(&"-".repeat(*w));
    }
    out.push('\n');

    // Rows.
    for row in rows {
        for (i, val) in row.values.iter().enumerate() {
            if i > 0 {
                out.push_str(" | ");
            }
            out.push_str(&format!("{:<w$}", val, w = widths.get(i).copied().unwrap_or(val.len())));
        }
        out.push('\n');
    }

    // Trailing newline is added by caller, so trim.
    out.trim_end_matches('\n').to_string()
}

fn help_text() -> String {
    "\
Commands:
  open <path>              Open a file
  readfile full|quick|x%   Block until file is read (full/quick/percentage)
  show [--plain]           Show current viewport
  goto <line>              Go to line N
  page <n>                 Go to page N
  home                     Jump to first line
  end                      Jump to last line (enables follow)
  follow on|off            Toggle follow mode
  severity <E|W|I|D|T> on|off   Toggle severity visibility
  search <pattern>         Set search pattern
  filter <expr>            Set filter expression
  sql <query>              Run SQL query against the in-memory DB
  histogram <bucket_secs>  Show time histogram of line counts
  stats                    Print statistics
  lines <from> <count>     Dump raw lines
  fields <line>            Show extracted fields for a line
  json <line>              Pretty-print JSON for a line
  help                     Show this help
  quit                     Exit
"
    .into()
}

/// Render the current viewport as text.
fn render_viewport(state: &AppState, plain: bool) -> String {
    if state.lines.is_empty() {
        return "(no lines yet)\n".into();
    }

    let visible = visible_lines(state);
    let mut out = String::with_capacity(visible.len() * 80);
    for (_no, sev, raw) in visible {
        if plain {
            out.push_str(&format!("{}\n", raw));
        } else {
            let color = match sev {
                "E" => "\x1b[31m",
                "W" => "\x1b[33m",
                "I" => "\x1b[36m",
                "D" => "\x1b[90m",
                "T" => "\x1b[90m",
                _ => "\x1b[0m",
            };
            out.push_str(&format!("{}{}\x1b[0m\n", color, raw));
        }
    }
    out
}

/// Return the visible lines as (line_no, severity_char, raw_text) tuples.
fn visible_lines(state: &AppState) -> Vec<(u64, &'static str, String)> {
    let vh = state.visible_height();
    let scroll = state.scroll.min(state.max_scroll());
    let mut out = Vec::with_capacity(vh);
    let mut idx = scroll;
    while out.len() < vh && idx < state.lines.len() {
        let pl = &state.lines[idx];
        idx += 1;
        if let Some(sev) = pl.severity
            && !state.severity_visible.is_visible(sev)
        {
            continue;
        }
        out.push((pl.line_no, severity_char(pl.severity), pl.raw.clone()));
    }
    out
}

/// Render specific lines by 1-based number.
fn render_lines(state: &AppState, from: u64, count: u64, _plain: bool) -> String {
    let mut out = String::new();
    let end = from.saturating_add(count);
    for i in from..end {
        let idx = line_to_index(state, i);
        match state.lines.get(idx) {
            Some(pl) => {
                out.push_str(&format!("{}\n", pl.raw));
            }
            None => break,
        }
    }
    out
}

fn render_fields(state: &AppState, line: u64) -> DispatchResult {
    let idx = line_to_index(state, line);
    match state.lines.get(idx) {
        Some(pl) => {
            let mut out = format!("line {}:\n", line);
            let mut keys: Vec<_> = pl.fields.keys().collect();
            keys.sort();
            for k in keys {
                let v = &pl.fields[k];
                out.push_str(&format!("  {} = {}\n", k, field_value_display(v)));
            }
            if let Some(ts) = pl.timestamp_ns {
                out.push_str(&format!("  _ts = {} ns\n", ts));
            }
            if let Some(sev) = pl.severity {
                out.push_str(&format!("  _severity = {:?}\n", sev));
            }
            DispatchResult::ok(out)
        }
        None => DispatchResult::ok(format!("error: line {} not found\n", line)),
    }
}

/// Convert a 1-based line number to a vector index.
/// Uses binary search since line numbers are monotonically increasing
/// after sorting by byte_offset (head lines 1..N, then tail lines
/// estimated_start..estimated_total, with a gap between them).
fn line_to_index(state: &AppState, line: u64) -> usize {
    if state.lines.is_empty() {
        return 0;
    }
    // Binary search for the line with the matching line_no.
    state
        .lines
        .partition_point(|pl| pl.line_no < line)
}

fn render_json(state: &AppState, line: u64) -> DispatchResult {
    let idx = line_to_index(state, line);
    match state.lines.get(idx).and_then(|pl| pl.json.as_ref()) {
        Some(v) => {
            let pretty = serde_json::to_string_pretty(v).unwrap_or_else(|e| format!("<error: {e}>"));
            DispatchResult::ok(format!("line {}:\n{}\n", line, pretty))
        }
        None => DispatchResult::ok(format!("error: line {} has no JSON\n", line)),
    }
}

fn severity_char(sev: Option<Severity>) -> &'static str {
    match sev {
        Some(Severity::Error) => "E",
        Some(Severity::Warn) => "W",
        Some(Severity::Info) => "I",
        Some(Severity::Debug) => "D",
        Some(Severity::Trace) => "T",
        None => " ",
    }
}

fn field_value_display(v: &crate::pipeline::parser::FieldValue) -> String {
    match v {
        crate::pipeline::parser::FieldValue::Null => "null".into(),
        crate::pipeline::parser::FieldValue::Bool(b) => b.to_string(),
        crate::pipeline::parser::FieldValue::Int(i) => i.to_string(),
        crate::pipeline::parser::FieldValue::Float(f) => f.to_string(),
        crate::pipeline::parser::FieldValue::Str(s) => format!("\"{s}\""),
    }
}

fn field_value_to_json(v: &crate::pipeline::parser::FieldValue) -> serde_json::Value {
    match v {
        crate::pipeline::parser::FieldValue::Null => serde_json::Value::Null,
        crate::pipeline::parser::FieldValue::Bool(b) => serde_json::Value::Bool(*b),
        crate::pipeline::parser::FieldValue::Int(i) => serde_json::Value::from(*i),
        crate::pipeline::parser::FieldValue::Float(f) => serde_json::Value::from(*f),
        crate::pipeline::parser::FieldValue::Str(s) => serde_json::Value::String(s.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::pipeline::parser::ParsedLine;
    use crate::theme::Theme;

    fn make_state(lines: Vec<ParsedLine>) -> AppState {
        let mut s = AppState::new(Config::default(), Theme::default_theme(), vec![], false);
        for l in lines {
            s.push_line(l);
        }
        s.terminal_height = 10;
        s
    }

    #[test]
    fn render_viewport_empty() {
        let s = make_state(vec![]);
        let out = render_viewport(&s, true);
        assert!(out.contains("no lines"));
    }

    #[test]
    fn render_viewport_with_lines() {
        let s = make_state(vec![
            ParsedLine::stub("hello"),
            ParsedLine::stub("world"),
        ]);
        let out = render_viewport(&s, true);
        assert!(out.contains("hello"));
        assert!(out.contains("world"));
    }

    #[test]
    fn severity_char_mapping() {
        assert_eq!(severity_char(Some(Severity::Error)), "E");
        assert_eq!(severity_char(None), " ");
    }
}
