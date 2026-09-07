//! File-backed line storage with LRU cache. Replaces the in-memory
//! `Vec<ParsedLine>` that caused OOM on huge files.
//!
//! The store holds:
//! - A shared `LineIndex` of byte offsets (built by the head reader).
//! - A bounded cache of parsed lines from the tail reader (end of file + follow).
//! - An LRU cache of lines read on demand from the file.
//! - A file handle and parser for on-demand reading.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use crate::io::line_index::LineIndex;
use crate::io::RawLine;
use crate::pipeline::parser::{Parser, ParsedLine};

/// Maximum number of tail lines kept in memory (from the tail reader).
const MAX_TAIL_LINES: usize = 5000;

/// Maximum number of on-demand cached lines (LRU).
const CACHE_CAPACITY: usize = 1000;

/// Read buffer size for on-demand line reading.
const READ_BUF_SIZE: usize = 64 * 1024;

/// A file-backed line store. Provides random access to lines by index
/// without loading the entire file into memory.
pub struct LineStore {
    /// Shared offset index (built by head reader, may still be growing).
    index: std::sync::Arc<LineIndex>,

    /// Lines from the tail reader (end of file + follow). Always in memory.
    /// Stored with their 0-based line index.
    tail_lines: VecDeque<(usize, ParsedLine)>,

    /// LRU cache of lines read from the file on demand.
    /// Keyed by 0-based line index.
    cache: HashMap<usize, ParsedLine>,
    lru_order: VecDeque<usize>,

    /// File handle for on-demand reading.
    pub file: Option<File>,

    /// Source path (for parser).
    source: String,

    /// Parser for on-demand line parsing.
    parser: Parser,

    /// Whether the store is for stdin (no file to seek back to).
    pub is_stdin: bool,

    /// Memory limit in bytes for ring buffer mode (0 = unlimited).
    /// When set, tail_lines are evicted by total estimated memory.
    memory_limit_bytes: usize,

    /// Estimated total memory used by tail_lines (bytes).
    tail_mem_bytes: usize,
}

impl LineStore {
    /// Create a new file-backed store.
    pub fn new(
        index: std::sync::Arc<LineIndex>,
        file: Option<File>,
        source: String,
        is_stdin: bool,
    ) -> Self {
        Self {
            index,
            tail_lines: VecDeque::new(),
            cache: HashMap::new(),
            lru_order: VecDeque::new(),
            file,
            source,
            parser: Parser::new(),
            is_stdin,
            memory_limit_bytes: 0,
            tail_mem_bytes: 0,
        }
    }

    /// Set a memory limit for the ring buffer (bytes). When set, tail lines
    /// are evicted by total estimated memory usage instead of just count.
    pub fn set_memory_limit(&mut self, limit_bytes: usize) {
        self.memory_limit_bytes = limit_bytes;
        self.evict_to_limit();
    }

    /// Total number of lines (from the offset index + tail lines beyond index).
    pub fn len(&self) -> usize {
        let index_len = self.index.len();
        // If tail has lines beyond the index, count those too.
        if let Some((last_idx, _)) = self.tail_lines.back() {
            (*last_idx + 1).max(index_len)
        } else {
            index_len
        }
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Push a parsed line from the tail reader / follow mode.
    /// The line's line_no is used to determine its 0-based index.
    pub fn push_tail(&mut self, line: ParsedLine) {
        let idx = (line.line_no as usize).saturating_sub(1);
        // Estimate memory: raw text + source + fields overhead.
        let est_bytes = estimate_line_bytes(&line);
        self.tail_lines.push_back((idx, line));
        self.tail_mem_bytes += est_bytes;
        // Evict oldest if over count limit or memory limit.
        self.evict_to_limit();
    }

    /// Evict oldest tail lines until under both count and memory limits.
    fn evict_to_limit(&mut self) {
        while self.tail_lines.len() > MAX_TAIL_LINES {
            if let Some((_, line)) = self.tail_lines.pop_front() {
                self.tail_mem_bytes = self.tail_mem_bytes.saturating_sub(estimate_line_bytes(&line));
            }
        }
        if self.memory_limit_bytes > 0 {
            while self.tail_mem_bytes > self.memory_limit_bytes && self.tail_lines.len() > 1 {
                if let Some((_, line)) = self.tail_lines.pop_front() {
                    self.tail_mem_bytes = self.tail_mem_bytes.saturating_sub(estimate_line_bytes(&line));
                }
            }
        }
    }

    /// Get a parsed line by 0-based index. Checks tail cache first, then
    /// LRU cache, then reads from the file on demand.
    pub fn get(&mut self, idx: usize) -> Option<ParsedLine> {
        let total = self.len();
        if idx >= total {
            return None;
        }

        // Check tail lines first (most likely for follow/end mode).
        if let Some(entry) = self.tail_lines.iter().find(|(i, _)| *i == idx) {
            return Some(entry.1.clone());
        }

        // Check LRU cache.
        if let Some(line) = self.cache.get(&idx) {
            // Update LRU order.
            self.lru_order.retain(|&i| i != idx);
            self.lru_order.push_back(idx);
            return Some(line.clone());
        }

        // Read from file on demand.
        if self.is_stdin {
            // Can't seek back in stdin. Only tail lines are available.
            return None;
        }

        self.read_from_file(idx)
    }

    /// Get a cached line without reading from file. Returns None if not
    /// in cache or tail lines.
    pub fn get_cached(&self, idx: usize) -> Option<&ParsedLine> {
        // Check tail lines.
        for (i, line) in &self.tail_lines {
            if *i == idx {
                return Some(line);
            }
        }
        // Check LRU cache.
        self.cache.get(&idx)
    }

    /// Get the byte offset of a line by index.
    pub fn offset(&self, idx: usize) -> Option<u64> {
        self.index.offset(idx)
    }

    /// Find the line index containing the given byte offset using binary
    /// search on the offset index.
    pub fn line_at_offset(&self, byte_offset: u64) -> usize {
        self.index.line_at_offset(byte_offset)
    }

    /// Read a line from the file at the given index, parse it, and cache it.
    fn read_from_file(&mut self, idx: usize) -> Option<ParsedLine> {
        let file = self.file.as_mut()?;
        let start_offset = self.index.offset(idx)?;
        let next_offset = self.index.offset(idx + 1);

        // Seek to the line start.
        if file.seek(SeekFrom::Start(start_offset)).is_err() {
            return None;
        }

        // Read until we find a newline or reach the next line's offset.
        let mut buf = vec![0u8; READ_BUF_SIZE];
        let mut line_bytes = Vec::new();
        let end = next_offset.unwrap_or(self.index.file_size());

        loop {
            let remaining = (end - start_offset - line_bytes.len() as u64) as usize;
            if remaining == 0 {
                break;
            }
            let to_read = buf.len().min(remaining);
            let n = match file.read(&mut buf[..to_read]) {
                Ok(n) => n,
                Err(_) => return None,
            };
            if n == 0 {
                break;
            }
            // Check for newline in the read data.
            let chunk = &buf[..n];
            if let Some(nl_pos) = chunk.iter().position(|&b| b == b'\n') {
                line_bytes.extend_from_slice(&chunk[..nl_pos]);
                break;
            } else {
                line_bytes.extend_from_slice(chunk);
            }
        }

        if line_bytes.is_empty() && next_offset == Some(start_offset) {
            return None;
        }

        // Convert to string (lossy for non-UTF-8).
        let raw = String::from_utf8_lossy(&line_bytes).into_owned();

        // Parse the line.
        let parsed = self.parser.parse(RawLine {
            source: self.source.clone(),
            byte_offset: start_offset,
            raw,
        });

        // Cache it.
        self.put_cache(idx, parsed.clone());

        Some(parsed)
    }

    /// Put a line into the LRU cache, evicting old entries if needed.
    fn put_cache(&mut self, idx: usize, line: ParsedLine) {
        if self.cache.len() >= CACHE_CAPACITY {
            // Evict least recently used.
            if let Some(old_idx) = self.lru_order.pop_front() {
                self.cache.remove(&old_idx);
            }
        }
        self.cache.insert(idx, line);
        self.lru_order.push_back(idx);
    }

    /// Clear all cached lines (e.g. when the file changes significantly).
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.lru_order.clear();
    }

    /// Get the last line number (1-based), or 0 if empty.
    pub fn last_line_no(&self) -> u64 {
        let total = self.len();
        if total == 0 {
            0
        } else {
            total as u64
        }
    }

    /// Iterate over tail lines (for search/filter in the tail region).
    pub fn tail_lines_iter(&self) -> impl Iterator<Item = (usize, &ParsedLine)> {
        self.tail_lines.iter().map(|(i, l)| (*i, l))
    }

    /// Whether the store has any tail lines.
    pub fn has_tail_lines(&self) -> bool {
        !self.tail_lines.is_empty()
    }

    /// Get the byte offset of the first tail line (for checking if a line
    /// is in the tail region).
    pub fn tail_start_offset(&self) -> Option<u64> {
        self.tail_lines.front().map(|(_, l)| l.byte_offset)
    }
}

/// Estimate the memory usage of a single ParsedLine in bytes.
/// This is an approximation — it counts the raw text, source string,
/// and field map overhead.
fn estimate_line_bytes(line: &ParsedLine) -> usize {
    let mut bytes = std::mem::size_of::<ParsedLine>();
    bytes += line.raw.capacity();
    bytes += line.source.capacity();
    // HashMap overhead: ~48 bytes base + each entry ~80 bytes.
    bytes += 48 + line.fields.len() * 80;
    // Each field key string.
    for (k, v) in &line.fields {
        bytes += k.capacity();
        bytes += match v {
            crate::pipeline::parser::FieldValue::Str(s) => s.capacity(),
            _ => 16,
        };
    }
    // JSON value (if present).
    if let Some(json) = &line.json {
        bytes += serde_json::to_string(json).map(|s| s.capacity()).unwrap_or(0);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn store_len_tracks_index_and_tail() {
        let index = Arc::new(LineIndex::new(100));
        index.push_offset(0);
        index.push_offset(10);
        let mut store = LineStore::new(index, None, "test".into(), false);
        assert_eq!(store.len(), 2);

        // Push a tail line beyond the index.
        let mut line = ParsedLine::stub("tail line");
        line.line_no = 5;
        store.push_tail(line);
        assert_eq!(store.len(), 5); // tail line at index 4 -> len = 5
    }

    #[test]
    fn tail_lines_are_capped() {
        let index = Arc::new(LineIndex::new(100));
        let mut store = LineStore::new(index, None, "test".into(), false);
        for i in 1..=MAX_TAIL_LINES + 100 {
            let mut line = ParsedLine::stub(&format!("line {i}"));
            line.line_no = i as u64;
            store.push_tail(line);
        }
        assert_eq!(store.tail_lines.len(), MAX_TAIL_LINES);
    }

    #[test]
    fn get_from_tail_lines() {
        let index = Arc::new(LineIndex::new(100));
        let mut store = LineStore::new(index, None, "test".into(), false);
        let mut line = ParsedLine::stub("hello");
        line.line_no = 3;
        store.push_tail(line);
        // Line 3 is at 0-based index 2.
        let got = store.get(2).unwrap();
        assert_eq!(got.raw, "hello");
    }

    #[test]
    fn empty_store_returns_none() {
        let index = Arc::new(LineIndex::new(0));
        let mut store = LineStore::new(index, None, "test".into(), false);
        assert!(store.is_empty());
        assert!(store.get(0).is_none());
    }

    #[test]
    fn read_from_file_on_demand() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("lr-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store_test.txt");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "alpha").unwrap();
        writeln!(f, "beta").unwrap();
        writeln!(f, "gamma").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let size = file.metadata().unwrap().len();

        let index = Arc::new(LineIndex::new(size));
        // Simulate head reader scanning for newlines.
        let _content = std::fs::read(&path).unwrap();
        index.push_offset(0); // "alpha" starts at 0
        // "alpha\n" = 6 bytes
        index.push_offset(6); // "beta" starts at 6
        // "beta\n" = 5 bytes
        index.push_offset(11); // "gamma" starts at 11
        index.set_head_done();

        let mut store = LineStore::new(index, Some(file), "test".into(), false);
        let line0 = store.get(0).unwrap();
        assert_eq!(line0.raw, "alpha");
        let line1 = store.get(1).unwrap();
        assert_eq!(line1.raw, "beta");
        let line2 = store.get(2).unwrap();
        assert_eq!(line2.raw, "gamma");
    }
}
