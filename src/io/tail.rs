//! Tail-follow reader. Starts at a given offset and polls for appends,
//! reading new bytes and sending complete lines on a tokio channel.
//!
//! Optionally performs an initial backward read: seeks to `EOF - N` bytes,
//! reads forward to EOF, and sends those lines before entering the follow
//! loop. This gives "tail -n" behavior — show the last screenful immediately
//! without reading the entire file.
//!
//! Uses polling (file-size check every 100ms) for reliability across
//! platforms. The `notify` crate can be used later for lower-latency
//! event-driven watching.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc::Sender;

use crate::io::line_splitter::LineSplitter;
use crate::io::RawLine;
use crate::pipeline::index::ReadProgress;

/// Poll interval for append detection when no watcher is used.
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

    loop {
        // Check if the consumer is still alive.
        if tx.is_closed() {
            tracing::debug!("tail: channel closed, exiting");
            return Ok(());
        }

        // Check current file size.
        let size = match std::fs::metadata(&path) {
            Ok(m) => m.len(),
            Err(e) => {
                tracing::warn!("tail: metadata error on {}: {e}", path.display());
                std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
                continue;
            }
        };

        let pos = file.stream_position()?;

        // Handle truncation (file shrank).
        if size < pos {
            tracing::info!(
                "tail: {} shrank ({} -> {}), re-seeking to 0",
                path.display(),
                pos,
                size
            );
            file.seek(SeekFrom::Start(0))?;
            splitter = LineSplitter::new(0);
            continue;
        }

        // Read any new bytes.
        if size > pos {
            loop {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                for frag in splitter.feed(&buf[..n]) {
                    if tx
                        .blocking_send(RawLine {
                            source: source.clone(),
                            byte_offset: frag.byte_offset,
                            raw: frag.raw,
                        })
                        .is_err()
                    {
                        tracing::debug!("tail: channel closed, exiting");
                        return Ok(());
                    }
                }
            }
        }

        std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
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
