//! Head reader: scans a file forward from byte 0, recording the byte offset
//! of each line start in a shared `LineIndex`. Does NOT send individual lines
//! through the parser pipeline — lines are read on demand from the file by
//! the `LineStore`. This prevents OOM on huge files.

use std::fs::File;
use std::io::Read;

use anyhow::Result;

use crate::io::line_index::LineIndex;
use crate::pipeline::index::ReadProgress;

/// Scan from `file` starting at byte 0 up to `end_offset`, recording the
/// byte offset of each line start in `index`. Updates `progress` as bytes
/// are read and newlines found.
///
/// Runs as a blocking task — use `tokio::task::spawn_blocking`.
pub fn head_reader(
    mut file: File,
    end_offset: u64,
    index: std::sync::Arc<LineIndex>,
    progress: ReadProgress,
) -> Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    let mut pos = 0u64;
    let mut at_line_start = true;
    let mut batch_offsets: Vec<u64> = Vec::with_capacity(1024);
    let mut line_count = 0u64;

    while pos < end_offset {
        let to_read = std::cmp::min(buf.len(), (end_offset - pos) as usize);
        let n = file.read(&mut buf[..to_read])?;
        if n == 0 {
            break; // unexpected EOF
        }

        for &byte in &buf[..n] {
            if at_line_start {
                batch_offsets.push(pos);
                at_line_start = false;
            }
            if byte == b'\n' {
                at_line_start = true;
                line_count += 1;
            }
            pos += 1;
        }

        // Flush batch to index periodically.
        if batch_offsets.len() >= 1024 {
            index.extend_offsets(&batch_offsets);
            batch_offsets.clear();
        }

        progress.record_bytes(n as u64, 0); // line count updated below
    }

    // Flush remaining offsets.
    if !batch_offsets.is_empty() {
        index.extend_offsets(&batch_offsets);
    }

    progress.record_bytes(0, line_count);
    progress.set_head_done();
    progress.set_lines_exact();
    index.set_head_done();
    tracing::debug!(
        "head_reader done: scanned {} bytes, found {} lines",
        pos,
        line_count
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn head_reader_builds_offset_index() {
        let dir = std::env::temp_dir().join(format!("lr-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("head_index_test.txt");
        std::fs::write(&path, b"alpha\nbeta\ngamma\n").unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let size = file.metadata().unwrap().len();
        let index = Arc::new(LineIndex::new(size));
        let progress = ReadProgress::new(size);

        let index_clone = index.clone();
        let handle = tokio::task::spawn_blocking(move || {
            head_reader(file, size, index_clone, progress)
        });
        handle.await.unwrap().unwrap();

        assert_eq!(index.len(), 3);
        assert_eq!(index.offset(0), Some(0)); // "alpha" at byte 0
        assert_eq!(index.offset(1), Some(6)); // "beta" at byte 6
        assert_eq!(index.offset(2), Some(11)); // "gamma" at byte 11
        assert!(index.head_done());
    }

    #[tokio::test]
    async fn head_reader_handles_no_trailing_newline() {
        let dir = std::env::temp_dir().join(format!("lr-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("head_no_nl.txt");
        std::fs::write(&path, b"line1\nline2\nno_newline").unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let size = file.metadata().unwrap().len();
        let index = Arc::new(LineIndex::new(size));
        let progress = ReadProgress::new(size);

        let index_clone = index.clone();
        let handle = tokio::task::spawn_blocking(move || {
            head_reader(file, size, index_clone, progress)
        });
        handle.await.unwrap().unwrap();

        // 3 lines: "line1", "line2", "no_newline"
        assert_eq!(index.len(), 3);
        assert_eq!(index.offset(0), Some(0));
        assert_eq!(index.offset(1), Some(6));
        assert_eq!(index.offset(2), Some(12));
    }

    #[tokio::test]
    async fn head_reader_updates_progress() {
        let dir = std::env::temp_dir().join(format!("lr-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("head_progress_test.txt");
        std::fs::write(&path, b"a\nb\nc\n").unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let size = file.metadata().unwrap().len();
        let index = Arc::new(LineIndex::new(size));
        let progress = ReadProgress::new(size);
        let p2 = progress.clone();

        let index_clone = index.clone();
        let handle = tokio::task::spawn_blocking(move || {
            head_reader(file, size, index_clone, progress)
        });
        handle.await.unwrap().unwrap();

        assert!(p2.head_done());
        assert_eq!(p2.bytes_read(), size);
        assert_eq!(p2.line_count(), 3);
    }
}
