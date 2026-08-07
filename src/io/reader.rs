//! Head reader: reads a file forward from byte 0 to a target end offset,
//! splitting bytes into lines and sending them on a tokio channel.

use std::fs::File;
use std::io::Read;

use anyhow::Result;
use tokio::sync::mpsc::Sender;

use crate::io::line_splitter::LineSplitter;
use crate::io::RawLine;

/// Read from `file` starting at byte 0 up to `end_offset`, splitting into
/// lines and sending each on `tx`. Stops at `end_offset` so that the tail
/// reader (which starts at `end_offset`) handles any appends.
///
/// Runs as a blocking task — use `tokio::task::spawn_blocking`.
pub fn head_reader(
    mut file: File,
    end_offset: u64,
    source: String,
    tx: Sender<RawLine>,
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
        for frag in splitter.feed(&buf[..n]) {
            if send_line(&tx, frag.byte_offset, &source, frag.raw) {
                return Ok(()); // channel closed (app quit)
            }
        }
    }

    // Flush any trailing partial line (file not ending with newline).
    if let Some(frag) = splitter.flush() {
        send_line(&tx, frag.byte_offset, &source, frag.raw);
    }

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

        let handle = tokio::task::spawn_blocking(move || {
            head_reader(file, size, "test".into(), tx)
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

        let handle = tokio::task::spawn_blocking(move || {
            head_reader(file, size, "test".into(), tx)
        });
        handle.await.unwrap().unwrap();

        let line = rx.recv().await.unwrap();
        assert_eq!(line.raw, "no_newline_here");
        assert!(rx.try_recv().is_err());
    }
}
