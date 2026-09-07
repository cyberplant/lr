//! Line offset index: maps line numbers to byte offsets in the file.
//!
//! The head reader scans the file for newlines and records the byte offset
//! of each line start. This index allows random access to any line by
//! seeking to its offset and reading until the next newline.
//!
//! Memory cost: 8 bytes per line (one `u64` offset). For a 63 GB file with
//! ~300M lines, the index is ~2.4 GB — far less than the ~63 GB that
//! storing full `ParsedLine` objects would require.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// Shared line offset index, built by the head reader and read by the UI.
///
/// The head reader pushes offsets as it scans. The UI reads `len()` for the
/// total line count and `offset(n)` to seek to a specific line.
pub struct LineIndex {
    /// Byte offset of the start of each line (0-based: line 0 = offsets[0]).
    offsets: Mutex<Vec<u64>>,

    /// Total file size (may grow for live files).
    file_size: AtomicU64,

    /// Whether the head reader has finished scanning.
    head_done: AtomicBool,
}

impl LineIndex {
    /// Create a new empty index for a file of the given size.
    pub fn new(file_size: u64) -> Self {
        Self {
            offsets: Mutex::new(Vec::new()),
            file_size: AtomicU64::new(file_size),
            head_done: AtomicBool::new(false),
        }
    }

    /// Push a line start offset. Called by the head reader as it scans.
    pub fn push_offset(&self, offset: u64) {
        let mut offsets = self.offsets.lock().unwrap();
        offsets.push(offset);
    }

    /// Push multiple line start offsets at once (batch, reduces lock contention).
    pub fn extend_offsets(&self, new_offsets: &[u64]) {
        let mut offsets = self.offsets.lock().unwrap();
        offsets.extend_from_slice(new_offsets);
    }

    /// Number of lines currently indexed.
    pub fn len(&self) -> usize {
        self.offsets.lock().unwrap().len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.offsets.lock().unwrap().is_empty()
    }

    /// Get the byte offset of line `idx` (0-based). Returns None if out of bounds.
    pub fn offset(&self, idx: usize) -> Option<u64> {
        let offsets = self.offsets.lock().unwrap();
        offsets.get(idx).copied()
    }

    /// Get the byte offsets of lines `start..end` (0-based). Returns a Vec.
    pub fn offsets_range(&self, start: usize, end: usize) -> Vec<u64> {
        let offsets = self.offsets.lock().unwrap();
        let end = end.min(offsets.len());
        if start >= end {
            return Vec::new();
        }
        offsets[start..end].to_vec()
    }

    /// Update the file size (e.g. when the file grows during follow).
    pub fn set_file_size(&self, size: u64) {
        self.file_size.store(size, Ordering::Relaxed);
    }

    /// Current known file size.
    pub fn file_size(&self) -> u64 {
        self.file_size.load(Ordering::Relaxed)
    }

    /// Mark the head reader as done scanning.
    pub fn set_head_done(&self) {
        self.head_done.store(true, Ordering::Relaxed);
    }

    /// Whether the head reader has finished scanning.
    pub fn head_done(&self) -> bool {
        self.head_done.load(Ordering::Relaxed)
    }

    /// Find the line index containing the given byte offset using binary search.
    /// Returns the 0-based line index, or 0 if the offset is before the first line.
    pub fn line_at_offset(&self, byte_offset: u64) -> usize {
        let offsets = self.offsets.lock().unwrap();
        if offsets.is_empty() {
            return 0;
        }
        // Find the last offset that is <= byte_offset.
        let idx = offsets.partition_point(|&o| o <= byte_offset);
        idx.saturating_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_grows_as_offsets_are_pushed() {
        let idx = LineIndex::new(100);
        idx.push_offset(0);
        idx.push_offset(10);
        idx.push_offset(20);
        assert_eq!(idx.len(), 3);
        assert_eq!(idx.offset(0), Some(0));
        assert_eq!(idx.offset(1), Some(10));
        assert_eq!(idx.offset(2), Some(20));
        assert_eq!(idx.offset(3), None);
    }

    #[test]
    fn line_at_offset_finds_correct_line() {
        let idx = LineIndex::new(100);
        idx.push_offset(0);
        idx.push_offset(10);
        idx.push_offset(20);
        idx.push_offset(30);
        // Byte 5 is in line 0 (offset 0).
        assert_eq!(idx.line_at_offset(5), 0);
        // Byte 10 is the start of line 1.
        assert_eq!(idx.line_at_offset(10), 1);
        // Byte 25 is in line 2 (offset 20).
        assert_eq!(idx.line_at_offset(25), 2);
        // Byte 0 is the start of line 0.
        assert_eq!(idx.line_at_offset(0), 0);
    }

    #[test]
    fn empty_index_returns_zero_for_line_at_offset() {
        let idx = LineIndex::new(0);
        assert_eq!(idx.line_at_offset(100), 0);
    }

    #[test]
    fn batch_extend_works() {
        let idx = LineIndex::new(100);
        idx.extend_offsets(&[0, 10, 20]);
        idx.extend_offsets(&[30, 40]);
        assert_eq!(idx.len(), 5);
        assert_eq!(idx.offset(3), Some(30));
    }
}
