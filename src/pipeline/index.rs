//! Line-offset index and read progress tracking.
//!
//! As the head reader scans the file, it records the byte offset of each
//! newline. This gives us:
//! - Total line count (without parsing every line)
//! - Fast seek to any line number (byte offset → seek)
//! - Read progress (bytes scanned vs file size) for the `readfile` command

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Shared read progress and line-offset index, updated by the head reader
/// and queried by the UI / REPL.
#[derive(Debug, Clone)]
pub struct ReadProgress {
    inner: Arc<ReadProgressInner>,
}

#[derive(Debug)]
struct ReadProgressInner {
    /// Bytes scanned by the head reader so far.
    head_bytes_read: AtomicU64,
    /// Total file size (initial, at open time).
    file_size: AtomicU64,
    /// Number of newlines found so far (= line count - 1 if no trailing NL).
    line_count: AtomicU64,
    /// Whether the head reader has finished (reached initial EOF).
    head_done: std::sync::atomic::AtomicBool,
    /// Estimated total line count, computed from the tail's 64KB chunk.
    /// 0 means not yet computed.
    estimated_total_lines: AtomicU64,
    /// Whether the line count is estimated (true) or exact (false).
    /// Estimated until the head reader reaches the tail's start offset.
    lines_estimated: std::sync::atomic::AtomicBool,
}

impl ReadProgress {
    pub fn new(file_size: u64) -> Self {
        Self {
            inner: Arc::new(ReadProgressInner {
                head_bytes_read: AtomicU64::new(0),
                file_size: AtomicU64::new(file_size),
                line_count: AtomicU64::new(0),
                head_done: std::sync::atomic::AtomicBool::new(false),
                estimated_total_lines: AtomicU64::new(0),
                lines_estimated: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }

    /// Update bytes read and line count. Called by the head reader.
    pub fn record_bytes(&self, bytes: u64, new_lines: u64) {
        self.inner
            .head_bytes_read
            .fetch_add(bytes, Ordering::Relaxed);
        self.inner.line_count.fetch_add(new_lines, Ordering::Relaxed);
    }

    /// Mark the head reader as done.
    pub fn set_head_done(&self) {
        self.inner.head_done.store(true, Ordering::Relaxed);
    }

    /// Bytes scanned by the head reader.
    pub fn bytes_read(&self) -> u64 {
        self.inner.head_bytes_read.load(Ordering::Relaxed)
    }

    /// Total file size.
    pub fn file_size(&self) -> u64 {
        self.inner.file_size.load(Ordering::Relaxed)
    }

    /// Approximate line count (newlines found so far).
    pub fn line_count(&self) -> u64 {
        self.inner.line_count.load(Ordering::Relaxed)
    }

    /// Whether the head reader has reached the initial EOF.
    pub fn head_done(&self) -> bool {
        self.inner.head_done.load(Ordering::Relaxed)
    }

    /// Progress as a fraction (0.0 to 1.0).
    pub fn fraction(&self) -> f64 {
        let size = self.file_size();
        if size == 0 {
            1.0
        } else {
            self.bytes_read() as f64 / size as f64
        }
    }

    /// Whether at least `target` fraction of the file has been read.
    pub fn reached_fraction(&self, target: f64) -> bool {
        self.fraction() >= target
    }

    /// Reset the progress tracker for a new file (used by the `open` REPL
    /// command when opening a different file dynamically).
    pub fn update_file_size(&self, size: u64) {
        self.inner.head_bytes_read.store(0, Ordering::Relaxed);
        self.inner.file_size.store(size, Ordering::Relaxed);
        self.inner.line_count.store(0, Ordering::Relaxed);
        self.inner.head_done.store(false, Ordering::Relaxed);
        self.inner.estimated_total_lines.store(0, Ordering::Relaxed);
        self.inner.lines_estimated.store(false, Ordering::Relaxed);
    }

    /// Set the estimated total line count, computed from the tail's 64KB chunk.
    /// Called by the tail reader after its initial backward read.
    pub fn set_estimated_total_lines(&self, estimated: u64) {
        self.inner.estimated_total_lines.store(estimated, Ordering::Relaxed);
        self.inner.lines_estimated.store(true, Ordering::Relaxed);
    }

    /// Mark the line count as exact (no longer estimated). Called when the
    /// head reader has scanned the entire file and we know the real count.
    pub fn set_lines_exact(&self) {
        self.inner.lines_estimated.store(false, Ordering::Relaxed);
    }

    /// The estimated total line count (0 if not yet computed).
    pub fn estimated_total_lines(&self) -> u64 {
        self.inner.estimated_total_lines.load(Ordering::Relaxed)
    }

    /// Whether the line count is currently estimated (not yet exact).
    pub fn lines_estimated(&self) -> bool {
        self.inner.lines_estimated.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_tracking() {
        let p = ReadProgress::new(1000);
        assert_eq!(p.bytes_read(), 0);
        assert_eq!(p.fraction(), 0.0);

        p.record_bytes(500, 10);
        assert_eq!(p.bytes_read(), 500);
        assert!((p.fraction() - 0.5).abs() < 0.001);
        assert!(p.reached_fraction(0.5));
        assert!(!p.reached_fraction(0.6));

        p.record_bytes(500, 10);
        assert_eq!(p.bytes_read(), 1000);
        assert!((p.fraction() - 1.0).abs() < 0.001);
    }

    #[test]
    fn zero_size_file_is_complete() {
        let p = ReadProgress::new(0);
        assert!((p.fraction() - 1.0).abs() < 0.001);
    }
}
