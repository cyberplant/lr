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
/// reader (which starts at `end_offset`) handles any appends.
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
    if let Some(frag) = splitter.flush() {
        progress.record_bytes(0, 1);
        send_line(&tx, frag.byte_offset, &source, frag.raw);
    }

    progress.set_head_done();
    tracing::debug!("head_reader done for {} at byte {}", source, pos);
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
        let dir = std::env::temp_dir().join(format!("lr-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("head_partial.txt");
        std::fs::write(&path, b"no_newline_here").unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let size = file.metadata().unwrap().len();
        let (tx, mut rx) = mpsc::channel::<RawLine>(100);
        let progress = ReadProgress::new(size);

        let handle = tokio::task::spawn_blocking(move || {
            head_reader(file, size, "test".into(), tx, progress)
        });
        handle.await.unwrap().unwrap();

        let line = rx.recv().await.unwrap();
        assert_eq!(line.raw, "no_newline_here");
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
