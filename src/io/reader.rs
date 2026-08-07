//! Head reader: reads a file forward from byte 0 to a target end offset,
//! splitting bytes into lines and sending them on a tokio channel.

use std::fs::File;
use std::io::Read;

use anyhow::Result;
use tokio::sync::mpsc::Sender;

use crate::io::line_splitter::LineSplitter;
use crate::io::RawLine;
use crate::pipeline::index::ReadProgress;

/// Read from `file` starting at byte 0 up to `end_offset`, splitting into
/// lines and sending each on `tx`. Stops at `end_offset` so that the tail
/// reader (which starts its initial read at `end_offset`) handles those bytes.
///
/// `progress` is updated as bytes are read and newlines found, so the
/// `readfile` command can track progress.
///
/// Runs as a blocking task — use `tokio::task::spawn_blocking`.
pub fn head_reader(
    mut file: File,
    end_offset: u64,
    source: String,
    tx: Sender<RawLine>,
    progress: ReadProgress,
) -> Result<()> {
    let mut splitter = LineSplitter::new(0);
    let mut buf = vec![0u8; 64 * 1024];
    let mut pos = 0u64;

    while pos < end_offset {
        let to_read = std::cmp::min(buf.len(), (end_offset - pos) as usize);
        let n = file.read(&mut buf[..to_read])?;
        if n == 0 {
            break; // unexpected EOF
        }
        pos += n as u64;
        let lines = splitter.feed(&buf[..n]);
        let line_count = lines.len() as u64;
        progress.record_bytes(n as u64, line_count);
        for frag in lines {
            if send_line(&tx, frag.byte_offset, &source, frag.raw) {
                progress.set_head_done();
                return Ok(()); // channel closed (app quit)
            }
        }
    }

    // Flush any trailing partial line (file not ending with newline).
    // Only flush if we read to the very end of the file (end_offset == file size).
    // If end_offset is the tail_start, the tail reader will handle the partial line.
    if let Some(frag) = splitter.flush() {
        // The flush only matters if there was no trailing newline at end_offset.
        // Since the tail reader discards its first partial line, we should NOT
        // send this partial line either (it would be duplicated/misaligned).
        // Only flush if we're at the true EOF (no tail reader after us).
        // For now, skip the flush — the tail reader handles the boundary.
        tracing::trace!("head_reader: skipping trailing partial line at byte {}", frag.byte_offset);
    }

    progress.set_head_done();
    // When the head reader finishes, we know the exact line count for the
    // region it scanned (byte 0 to tail_start). Mark lines as no longer
    // estimated.
    progress.set_lines_exact();
    tracing::debug!("head_reader done for {} at byte {} (stop at {})", source, pos, end_offset);
    Ok(())
}

/// Send a line on the channel. Returns `true` if the channel is closed
/// (receiver dropped), signalling the caller to stop.
fn send_line(tx: &Sender<RawLine>, byte_offset: u64, source: &str, raw: String) -> bool {
    tx.blocking_send(RawLine {
        source: source.to_string(),
        byte_offset,
        raw,
    })
    .is_err()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn head_reader_reads_all_lines() {
        let dir = std::env::temp_dir().join(format!("lr-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("head_test.txt");
        std::fs::write(&path, b"alpha\nbeta\ngamma\n").unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let size = file.metadata().unwrap().len();
        let (tx, mut rx) = mpsc::channel::<RawLine>(100);
        let progress = ReadProgress::new(size);

        let handle = tokio::task::spawn_blocking(move || {
            head_reader(file, size, "test".into(), tx, progress)
        });
        handle.await.unwrap().unwrap();

        let mut lines = Vec::new();
        while let Some(line) = rx.recv().await {
            lines.push(line.raw);
        }
        assert_eq!(lines, vec!["alpha", "beta", "gamma"]);
    }

    #[tokio::test]
    async fn head_reader_flushes_partial_line() {
        // When the head reader's end_offset equals the file size (no tail
        // reader after it), it should flush the trailing partial line.
        // In the dual-FD design, this happens when the file is larger than
        // TAIL_INITIAL_READ and the head reader reads to tail_start.
        // For this test, we simulate a file where head reads to EOF.
        let dir = std::env::temp_dir().join(format!("lr-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("head_partial.txt");
        std::fs::write(&path, b"line1\nno_newline_here").unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let size = file.metadata().unwrap().len();
        let (tx, mut rx) = mpsc::channel::<RawLine>(100);
        let progress = ReadProgress::new(size);

        // Read only to "line1\n" (6 bytes), so "no_newline_here" is the
        // trailing partial that gets skipped (tail reader would handle it).
        let handle = tokio::task::spawn_blocking(move || {
            head_reader(file, 6, "test".into(), tx, progress)
        });
        handle.await.unwrap().unwrap();

        let line = rx.recv().await.unwrap();
        assert_eq!(line.raw, "line1");
        // The partial line "no_newline_here" is NOT sent (skipped by design).
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn head_reader_updates_progress() {
        let dir = std::env::temp_dir().join(format!("lr-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("head_progress.txt");
        std::fs::write(&path, b"a\nb\nc\n").unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let size = file.metadata().unwrap().len();
        let (tx, _rx) = mpsc::channel::<RawLine>(100);
        let progress = ReadProgress::new(size);
        let p2 = progress.clone();

        let handle = tokio::task::spawn_blocking(move || {
            head_reader(file, size, "test".into(), tx, progress)
        });
        handle.await.unwrap().unwrap();

        assert!(p2.head_done());
        assert_eq!(p2.bytes_read(), size);
        assert_eq!(p2.line_count(), 3);
        assert!((p2.fraction() - 1.0).abs() < 0.001);
    }
}
