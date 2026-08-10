//! Tail-follow reader. Starts at a given offset and follows appends,
//! reading new bytes and sending complete lines on a tokio channel.
//!
//! Optionally performs an initial backward read: seeks to `EOF - N` bytes,
//! reads forward to EOF, and sends those lines before entering the follow
//! loop. This gives "tail -n" behavior — show the last screenful immediately
//! without reading the entire file.
//!
//! Uses the `notify` crate for event-driven append detection (FSEvents on
//! macOS, inotify on Linux, ReadDirectoryChangesW on Windows). Falls back
//! to polling (file-size check every 100ms) if the watcher fails to start.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc::Sender;

use crate::io::line_splitter::LineSplitter;
use crate::io::RawLine;
use crate::pipeline::index::ReadProgress;

/// Poll interval for append detection when falling back to polling.
const POLL_INTERVAL_MS: u64 = 100;

/// Default bytes to read backward from EOF for the initial tail view.
/// 64KB is enough for ~500 typical log lines.
const DEFAULT_INITIAL_READ: u64 = 64 * 1024;

/// Read from `file` starting at `start_offset`, following appends
/// indefinitely. Sends each new complete line on `tx`.
///
/// Runs as a blocking task — use `tokio::task::spawn_blocking`.
pub fn tail_reader(
    file: File,
    start_offset: u64,
    path: PathBuf,
    source: String,
    tx: Sender<RawLine>,
) -> Result<()> {
    tail_reader_with_initial(file, start_offset, path, source, tx, 0, None)
}

/// Like `tail_reader` but with an initial backward read of `initial_read_bytes`
/// bytes from `start_offset`. If `progress` is given, computes an estimated
/// total line count from the initial read and stores it.
pub fn tail_reader_with_initial(
    mut file: File,
    start_offset: u64,
    path: PathBuf,
    source: String,
    tx: Sender<RawLine>,
    initial_read_bytes: u64,
    progress: Option<ReadProgress>,
) -> Result<()> {
    // Initial backward read: seek to (start_offset - N), read forward to
    // start_offset, send those lines, then enter the follow loop.
    if initial_read_bytes > 0 && start_offset > 0 {
        let seek_pos = start_offset.saturating_sub(initial_read_bytes);
        file.seek(SeekFrom::Start(seek_pos))?;

        let bytes_to_read = start_offset - seek_pos;
        let mut splitter = LineSplitter::new(seek_pos);
        let mut buf = vec![0u8; 64 * 1024];
        let mut read_so_far = 0u64;
        let mut first_line = true;
        let mut lines_sent = 0u64;

        while read_so_far < bytes_to_read {
            let to_read = std::cmp::min(buf.len(), (bytes_to_read - read_so_far) as usize);
            let n = file.read(&mut buf[..to_read])?;
            if n == 0 {
                break;
            }
            read_so_far += n as u64;
            for frag in splitter.feed(&buf[..n]) {
                // If we started mid-line (seek_pos > 0), the first "line"
                // from the splitter is a partial line — discard it.
                if first_line && seek_pos > 0 {
                    first_line = false;
                    continue;
                }
                first_line = false;
                lines_sent += 1;
                if tx
                    .blocking_send(RawLine {
                        source: source.clone(),
                        byte_offset: frag.byte_offset,
                        raw: frag.raw,
                    })
                    .is_err()
                {
                    tracing::debug!("tail: channel closed during initial read");
                    return Ok(());
                }
            }
        }

        // Flush any trailing partial line from the initial read.
        // (This shouldn't happen since we read exactly to start_offset,
        // but handle it just in case.)
        if let Some(frag) = splitter.flush()
            && !(first_line && seek_pos > 0)
        {
            let _ = tx.blocking_send(RawLine {
                source: source.clone(),
                byte_offset: frag.byte_offset,
                raw: frag.raw,
            });
        }

        tracing::debug!(
            "tail: initial read {} bytes from offset {}, {} bytes before EOF",
            read_so_far,
            seek_pos,
            start_offset - seek_pos
        );

        // Compute estimated total line count from the initial read.
        // Count newlines in the bytes we read, compute average line size,
        // and extrapolate to the full file.
        if let Some(ref progress) = progress {
            let lines_in_chunk = lines_sent;
            let bytes_in_chunk = read_so_far;
            if bytes_in_chunk > 0 && lines_in_chunk > 0 {
                let avg_line_size = bytes_in_chunk as f64 / lines_in_chunk as f64;
                let estimated_total = (start_offset as f64 / avg_line_size).round() as u64;
                progress.set_estimated_total_lines(estimated_total + lines_in_chunk);
                tracing::debug!(
                    "tail: estimated {} total lines (avg line size: {:.1} bytes, {} lines in {} bytes)",
                    estimated_total + lines_in_chunk,
                    avg_line_size,
                    lines_in_chunk,
                    bytes_in_chunk
                );
            }
        }
    }

    // Seek to start offset for the follow loop.
    file.seek(SeekFrom::Start(start_offset))?;

    let mut splitter = LineSplitter::new(start_offset);
    let mut buf = vec![0u8; 64 * 1024];

    // Set up the notify watcher. We watch the file itself; notify will use
    // FSEvents (macOS), inotify (Linux), or ReadDirectoryChangesW (Windows).
    // If the watcher fails to start, we fall back to polling.
    let (notify_tx, notify_rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
    let watcher_result: anyhow::Result<RecommendedWatcher> = (|| {
        let w = notify::recommended_watcher(notify_tx)
            .map_err(|e| anyhow::anyhow!("create watcher: {e}"))?;
        Ok(w)
    })();

    let watcher = match watcher_result {
        Ok(mut w) => {
            // Watch the parent directory (non-recursive) so we also catch
            // file rotation/recreation events that replace the file.
            let watch_path = path.parent().unwrap_or(&path);
            match w.watch(watch_path, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    tracing::debug!("tail: watching {} for changes", watch_path.display());
                    Some(w)
                }
                Err(e) => {
                    tracing::warn!("tail: watch failed on {}, falling back to polling: {e}", watch_path.display());
                    None
                }
            }
        }
        Err(e) => {
            tracing::warn!("tail: could not create watcher, falling back to polling: {e}");
            None
        }
    };

    // `read_new_bytes` helper: reads any new bytes from the current file
    // position, sends complete lines, handles truncation, and updates
    // estimated line count. Returns false if the channel is closed.
    let read_new_bytes = |file: &mut File,
                          splitter: &mut LineSplitter,
                          buf: &mut [u8],
                          tx: &Sender<RawLine>,
                          source: &str,
                          progress: &Option<ReadProgress>|
     -> bool {
        let size = match std::fs::metadata(&path) {
            Ok(m) => m.len(),
            Err(e) => {
                tracing::warn!("tail: metadata error on {}: {e}", path.display());
                return true; // keep going
            }
        };

        let pos = match file.stream_position() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("tail: stream_position error: {e}");
                return true;
            }
        };

        // Handle truncation (file shrank).
        if size < pos {
            tracing::info!(
                "tail: {} shrank ({} -> {}), re-seeking to 0",
                path.display(),
                pos,
                size
            );
            if file.seek(SeekFrom::Start(0)).is_err() {
                return true;
            }
            *splitter = LineSplitter::new(0);
            return true;
        }

        if size <= pos {
            return true; // nothing new
        }

        let mut new_lines = 0u64;
        loop {
            let n = match file.read(buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!("tail: read error: {e}");
                    break;
                }
            };
            for frag in splitter.feed(&buf[..n]) {
                if tx
                    .blocking_send(RawLine {
                        source: source.to_string(),
                        byte_offset: frag.byte_offset,
                        raw: frag.raw,
                    })
                    .is_err()
                {
                    tracing::debug!("tail: channel closed, exiting");
                    return false;
                }
                new_lines += 1;
            }
        }
        if new_lines > 0
            && let Some(progress) = progress
        {
            progress.increment_total_lines(new_lines);
        }
        true
    };

    // Initial drain: read any bytes that arrived between the initial read
    // and the watcher starting.
    if !read_new_bytes(&mut file, &mut splitter, &mut buf, &tx, &source, &progress) {
        return Ok(());
    }

    if watcher.is_some() {
        // Event-driven loop: wait for notify events, then read new bytes.
        // We also poll periodically as a safety net (some platforms may
        // coalesce or drop events).
        let poll_timeout = Duration::from_millis(POLL_INTERVAL_MS);
        loop {
            if tx.is_closed() {
                tracing::debug!("tail: channel closed, exiting");
                return Ok(());
            }

            // Wait for a notify event or timeout.
            match notify_rx.recv_timeout(poll_timeout) {
                Ok(Ok(_event)) => {
                    if !read_new_bytes(&mut file, &mut splitter, &mut buf, &tx, &source, &progress) {
                        return Ok(());
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!("tail: watcher error: {e}");
                    // Still try to read in case there are new bytes.
                    if !read_new_bytes(&mut file, &mut splitter, &mut buf, &tx, &source, &progress) {
                        return Ok(());
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // No event — do a quick check as a safety net.
                    if !read_new_bytes(&mut file, &mut splitter, &mut buf, &tx, &source, &progress) {
                        return Ok(());
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    tracing::debug!("tail: watcher disconnected, exiting");
                    return Ok(());
                }
            }
        }
    } else {
        // Polling fallback loop.
        loop {
            if tx.is_closed() {
                tracing::debug!("tail: channel closed, exiting");
                return Ok(());
            }
            if !read_new_bytes(&mut file, &mut splitter, &mut buf, &tx, &source, &progress) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn tail_reader_follows_appends() {
        let dir = std::env::temp_dir().join(format!("lr-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tail_test.txt");
        std::fs::write(&path, b"existing\n").unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let start = file.metadata().unwrap().len();

        let (tx, rx) = mpsc::channel::<RawLine>(100);
        let path_clone = path.clone();

        let handle = tokio::task::spawn_blocking(move || {
            tail_reader(file, start, path_clone, "test".into(), tx)
        });

        // Append a line after a short delay.
        std::thread::sleep(Duration::from_millis(200));
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "appended!").unwrap();

        // Use a separate task to receive the line so we can timeout.
        let mut rx = rx;
        let line = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");
        assert_eq!(line.raw, "appended!");

        // Drop the receiver to close the channel, causing the tail reader
        // to exit on its next blocking_send.
        drop(rx);
        // Give the tail reader time to notice the closed channel.
        let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
    }

    #[tokio::test]
    async fn tail_initial_read_shows_last_lines() {
        let dir = std::env::temp_dir().join(format!("lr-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tail_initial.txt");
        // Write 100 lines, each ~10 bytes ("line 000\n")
        let content: String = (0..100).map(|i| format!("line {:03}\n", i)).collect();
        std::fs::write(&path, content.as_bytes()).unwrap();
        let file_size = content.len() as u64;

        let file = std::fs::File::open(&path).unwrap();
        let (tx, rx) = mpsc::channel::<RawLine>(1000);
        let path_clone = path.clone();

        // Read last 200 bytes (should give us ~20 lines)
        let handle = tokio::task::spawn_blocking(move || {
            tail_reader_with_initial(file, file_size, path_clone, "test".into(), tx, 200, None)
        });

        let mut rx = rx;
        let mut lines = Vec::new();
        // Collect lines with a timeout — the initial read finishes quickly,
        // then the follow loop starts (which won't get any new lines).
        while let Ok(Some(line)) =
            tokio::time::timeout(Duration::from_millis(500), rx.recv()).await
        {
            lines.push(line.raw);
        }

        // We should have gotten the last ~20 lines (not line 000, but line ~080+)
        assert!(!lines.is_empty(), "should have gotten some lines");
        assert!(lines.len() < 100, "should not have all 100 lines");
        // Last line should be "line 099"
        assert_eq!(lines.last().unwrap(), "line 099");
        // First line should NOT be "line 000" (we started from the end)
        assert_ne!(lines.first().unwrap(), "line 000");

        drop(rx);
        let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
    }

    #[tokio::test]
    async fn tail_initial_read_small_file() {
        // File smaller than initial_read_bytes — should read from byte 0
        let dir = std::env::temp_dir().join(format!("lr-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tail_small.txt");
        std::fs::write(&path, b"a\nb\nc\n").unwrap();
        let file_size = 6;

        let file = std::fs::File::open(&path).unwrap();
        let (tx, rx) = mpsc::channel::<RawLine>(100);
        let path_clone = path.clone();

        let handle = tokio::task::spawn_blocking(move || {
            tail_reader_with_initial(file, file_size, path_clone, "test".into(), tx, 1024, None)
        });

        let mut rx = rx;
        let mut lines = Vec::new();
        while let Ok(Some(line)) =
            tokio::time::timeout(Duration::from_millis(500), rx.recv()).await
        {
            lines.push(line.raw);
        }

        // Small file: seek_pos = 0, so no partial line to discard.
        // Should get all 3 lines.
        assert_eq!(lines, vec!["a", "b", "c"]);

        drop(rx);
        let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
    }
}
