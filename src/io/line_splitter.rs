//! Buffered line splitter. Accumulates bytes and emits complete lines
//! (terminated by `\n`). Handles chunk boundaries that fall mid-line and
//! strips trailing `\r` for CRLF compatibility.

/// A complete line extracted from the byte stream.
pub struct LineFragment {
    /// Byte offset of the start of this line in the source.
    pub byte_offset: u64,
    /// Line text without the trailing newline (or `\r`).
    pub raw: String,
}

pub struct LineSplitter {
    buf: Vec<u8>,
    /// Byte offset of the start of the next line to be emitted.
    next_offset: u64,
}

impl LineSplitter {
    pub fn new(start_offset: u64) -> Self {
        Self {
            buf: Vec::with_capacity(4096),
            next_offset: start_offset,
        }
    }

    /// Feed a chunk of bytes and return all complete lines found.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<LineFragment> {
        let mut lines = Vec::new();
        for &byte in chunk {
            self.buf.push(byte);
            if byte == b'\n' {
                let line_bytes = std::mem::take(&mut self.buf);
                let offset = self.next_offset;
                self.next_offset += line_bytes.len() as u64;
                // Strip trailing \n, and \r if present (CRLF).
                let end = line_bytes.len() - 1;
                let end = if end > 0 && line_bytes[end - 1] == b'\r' {
                    end - 1
                } else {
                    end
                };
                let text = String::from_utf8_lossy(&line_bytes[..end]).into_owned();
                lines.push(LineFragment {
                    byte_offset: offset,
                    raw: text,
                });
            }
        }
        lines
    }

    /// Return any remaining buffered bytes as a line (for EOF without a
    /// trailing newline). Returns `None` if the buffer is empty.
    pub fn flush(&mut self) -> Option<LineFragment> {
        if self.buf.is_empty() {
            return None;
        }
        let line_bytes = std::mem::take(&mut self.buf);
        let offset = self.next_offset;
        self.next_offset += line_bytes.len() as u64;
        // Strip trailing \r if present.
        let end = if line_bytes.last() == Some(&b'\r') {
            line_bytes.len() - 1
        } else {
            line_bytes.len()
        };
        let text = String::from_utf8_lossy(&line_bytes[..end]).into_owned();
        Some(LineFragment {
            byte_offset: offset,
            raw: text,
        })
    }

    /// Current byte offset (start of the next line to be emitted).
    pub fn current_offset(&self) -> u64 {
        self.next_offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_complete_lines() {
        let mut s = LineSplitter::new(0);
        let lines = s.feed(b"hello\nworld\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].raw, "hello");
        assert_eq!(lines[0].byte_offset, 0);
        assert_eq!(lines[1].raw, "world");
        assert_eq!(lines[1].byte_offset, 6);
    }

    #[test]
    fn handles_chunk_boundary() {
        let mut s = LineSplitter::new(0);
        let lines = s.feed(b"hel");
        assert!(lines.is_empty());
        let lines = s.feed(b"lo\nwor");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].raw, "hello");
        let lines = s.feed(b"ld\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].raw, "world");
    }

    #[test]
    fn strips_crlf() {
        let mut s = LineSplitter::new(0);
        let lines = s.feed(b"line\r\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].raw, "line");
        assert_eq!(lines[0].byte_offset, 0);
        // Offset advances by 6 (including \r\n)
        assert_eq!(s.current_offset(), 6);
    }

    #[test]
    fn flush_incomplete_line() {
        let mut s = LineSplitter::new(0);
        s.feed(b"no newline");
        let last = s.flush().unwrap();
        assert_eq!(last.raw, "no newline");
        assert!(s.flush().is_none());
    }

    #[test]
    fn empty_flush_returns_none() {
        let mut s = LineSplitter::new(100);
        assert!(s.flush().is_none());
        assert_eq!(s.current_offset(), 100);
    }
}
