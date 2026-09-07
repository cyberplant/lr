//! Stdin source: reads stdin and either spills to a temp file (default)
//! or streams into a channel for in-memory ring buffer mode.
//!
//! In temp-file mode, stdin is written to a temp file (preferring
//! `/dev/shm` for RAM-backed storage on Linux) while simultaneously
//! building a `LineIndex` of byte offsets. This enables full random
//! access to all stdin data via the same `LineStore` used for regular
//! files, with bounded memory (8 bytes per line for the offset index).
//!
//! In memory mode, stdin lines are sent through the channel as before,
//! and the `LineStore` keeps them in a bounded ring buffer.

use std::fs::File;
use std::io::{BufRead, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc::Sender;

use crate::io::line_index::LineIndex;
use crate::io::RawLine;
use crate::pipeline::index::ReadProgress;

/// Read buffer size for stdin spilling.
const READ_BUF_SIZE: usize = 64 * 1024;

/// Result of stdin spilling: the temp file path and handle, plus the
/// final file size.
pub struct StdinSpillResult {
    /// Path to the temp file (for cleanup on exit).
    pub path: PathBuf,
    /// Open file handle for the LineStore to read from.
    pub file: File,
    /// Total bytes written.
    pub size: u64,
}

/// Choose the best temp directory for stdin spilling. Prefers `/dev/shm`
/// (RAM-backed tmpfs on Linux) if available and writable, then falls back
/// to the system temp dir.
fn best_temp_dir() -> PathBuf {
    // On Linux, /dev/shm is RAM-backed (tmpfs) and ideal for spilling.
    let shm = std::path::Path::new("/dev/shm");
    if shm.is_dir() {
        // Verify we can actually write to it.
        let test = shm.join(format!(".lr_write_test_{}", std::process::id()));
        if std::fs::write(&test, b"").is_ok() {
            let _ = std::fs::remove_file(&test);
            return shm.to_path_buf();
        }
    }
    // Fall back to the system temp dir.
    std::env::temp_dir()
}

/// Create a temp file for stdin spilling. Returns the path and open handle.
fn create_temp_file() -> Result<(PathBuf, File)> {
    let dir = best_temp_dir();
    let pid = std::process::id();
    // Use a unique name to avoid collisions.
    let path = dir.join(format!("lr-stdin-{pid}.log"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(&path)?;
    Ok((path, file))
}

/// Read stdin, spill to a temp file, and build a byte offset index.
///
/// This function:
/// 1. Creates a temp file (preferring /dev/shm).
/// 2. Reads stdin in 64KB chunks.
/// 3. Writes each chunk to the temp file.
/// 4. Scans for newlines and records byte offsets in the `LineIndex`.
/// 5. Updates `progress` as bytes are read.
///
/// After this completes, the `LineStore` can use the temp file + index
/// for on-demand line reading, just like a regular file.
///
/// Runs as a blocking task — use `tokio::task::spawn_blocking`.
pub fn stdin_to_temp_file(
    index: Arc<LineIndex>,
    progress: ReadProgress,
) -> Result<StdinSpillResult> {
    let (path, file) = create_temp_file()?;
    let mut writer = BufWriter::new(file.try_clone()?);

    let stdin = std::io::stdin();
    let mut reader = stdin.lock();

    let mut buf = vec![0u8; READ_BUF_SIZE];
    let mut pos = 0u64;
    let mut at_line_start = true;
    let mut batch_offsets: Vec<u64> = Vec::with_capacity(1024);
    let mut line_count = 0u64;

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break; // EOF
        }

        // Write to temp file.
        writer.write_all(&buf[..n])?;

        // Scan for line offsets.
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

        progress.record_bytes(n as u64, 0);
    }

    // Flush remaining offsets and the writer.
    if !batch_offsets.is_empty() {
        index.extend_offsets(&batch_offsets);
    }
    writer.flush()?;
    drop(writer);

    progress.record_bytes(0, line_count);
    progress.set_head_done();
    progress.set_lines_exact();
    index.set_file_size(pos);
    index.set_head_done();

    tracing::debug!(
        "stdin_to_temp_file: spilled {} bytes, {} lines to {}",
        pos,
        line_count,
        path.display()
    );

    Ok(StdinSpillResult {
        path,
        file,
        size: pos,
    })
}

/// Read stdin line-by-line and send each line on `tx` (for memory/ring
/// buffer mode). This is the original stdin reader behavior.
///
/// Runs as a blocking task — use `tokio::task::spawn_blocking`.
pub fn stdin_reader(tx: Sender<RawLine>) -> Result<()> {
    let stdin = std::io::stdin();
    let reader = std::io::BufReader::new(stdin.lock());
    let mut offset = 0u64;

    for line in reader.lines() {
        let line = line?;
        let line_bytes = line.len() as u64 + 1; // +1 for newline
        if tx
            .blocking_send(RawLine {
                source: "stdin".to_string(),
                byte_offset: offset,
                raw: line,
            })
            .is_err()
        {
            break; // channel closed (app quit)
        }
        offset += line_bytes;
    }

    tracing::debug!("stdin_reader done at byte {}", offset);
    Ok(())
}

/// Clean up a temp file created by stdin spilling. Safe to call even if
/// the file was already removed.
pub fn cleanup_temp_file(path: &PathBuf) {
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!("failed to clean up stdin temp file {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_temp_dir_returns_a_valid_dir() {
        let dir = best_temp_dir();
        assert!(dir.is_dir(), "temp dir should exist: {}", dir.display());
    }

    #[test]
    fn create_temp_file_creates_and_truncates() {
        let (path, file) = create_temp_file().unwrap();
        assert!(path.exists());
        assert!(file.metadata().is_ok());
        cleanup_temp_file(&path);
        assert!(!path.exists());
    }

    #[test]
    fn cleanup_temp_file_is_idempotent() {
        let (path, _) = create_temp_file().unwrap();
        cleanup_temp_file(&path);
        // Second call should not panic.
        cleanup_temp_file(&path);
    }
}
