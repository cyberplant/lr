//! File and stream I/O: dual-FD open, head/tail/stdin readers, line buffer,
//! line offset index.

pub mod file;
pub mod line_buffer;
pub mod line_index;
pub mod line_splitter;
pub mod reader;
pub mod stdin;
pub mod tail;

/// A raw line read from a source, before parsing.
#[derive(Debug, Clone)]
pub struct RawLine {
    /// Source tag (file path or "stdin").
    pub source: String,
    /// Byte offset of the start of this line in the source.
    pub byte_offset: u64,
    /// Raw line text (no trailing newline).
    pub raw: String,
}
