//! Bounded ring buffer of parsed lines with backpressure.
//!
//! Stub for phase 0; full implementation in phase 1.

use crate::pipeline::parser::ParsedLine;

/// A bounded buffer of parsed lines. When full, producers should pause
/// reading (tokio channel backpressure handles this naturally).
pub struct LineBuffer {
    capacity: usize,
    lines: std::collections::VecDeque<ParsedLine>,
}

impl LineBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            lines: std::collections::VecDeque::with_capacity(capacity),
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn push_back(&mut self, line: ParsedLine) {
        if self.lines.len() >= self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_evicts_oldest_when_full() {
        let mut b = LineBuffer::new(2);
        b.push_back(ParsedLine::stub("a"));
        b.push_back(ParsedLine::stub("b"));
        b.push_back(ParsedLine::stub("c"));
        assert_eq!(b.len(), 2);
    }
}
