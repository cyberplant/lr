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
}

impl ReadProgress {
    pub fn new(file_size: u64) -> Self {
        Self {
            inner: Arc::new(ReadProgressInner {
                head_bytes_read: AtomicU64::new(0),
                file_size: AtomicU64::new(file_size),
                line_count: AtomicU64::new(0),
                head_done: std::sync::atomic::AtomicBool::new(false),
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
