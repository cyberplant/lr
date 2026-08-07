//! Tail-follow reader. Starts at a given offset and polls for appends,
//! reading new bytes and sending complete lines on a tokio channel.
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

/// Poll interval for append detection when no watcher is used.
const POLL_INTERVAL_MS: u64 = 100;

/// Read from `file` starting at `start_offset`, following appends
/// indefinitely. Sends each new complete line on `tx`.
///
/// Runs as a blocking task — use `tokio::task::spawn_blocking`.
pub fn tail_reader(
    mut file: File,
    start_offset: u64,
    path: PathBuf,
    source: String,
    tx: Sender<RawLine>,
) -> Result<()> {
    // Seek to start offset (should already be there from open_dual, but be safe).
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
}
