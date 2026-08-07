//! Opening a file with two file descriptors: a head reader streaming from
//! byte 0, and a tail reader holding the end position and following appends.
//!
//! Both readers run simultaneously. The head reader stops when it reaches
//! the tail reader's start offset, so there are no duplicate lines.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};

/// Default bytes to read backward from EOF for the initial tail view.
/// 64KB is enough for ~500 typical log lines.
pub const TAIL_INITIAL_READ: u64 = 64 * 1024;

/// A pair of handles to the same file: one reading forward from the start,
/// one positioned at the end ready to follow appends.
pub struct DualFd {
    pub head: File,
    pub tail: File,
    pub size: u64,
    /// The byte offset where the tail reader begins its initial backward read.
    /// The head reader should stop at this offset to avoid duplicates.
    pub tail_start: u64,
}

/// Open `path` twice and position the head at byte 0 and the tail at EOF.
/// The tail's initial read starts at `max(0, size - TAIL_INITIAL_READ)`.
pub fn open_dual(path: &Path) -> Result<DualFd> {
    let head = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut tail = std::fs::File::open(path)
        .with_context(|| format!("open (tail) {}", path.display()))?;
    let size = head
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    // Tail starts at EOF, but will do an initial backward read from
    // (size - TAIL_INITIAL_READ) to size before entering the follow loop.
    let tail_start = size.saturating_sub(TAIL_INITIAL_READ);
    // Seek tail to end (the follow position).
    use std::io::Seek;
    tail.seek(std::io::SeekFrom::End(0))
        .with_context(|| format!("seek to end {}", path.display()))?;
    tracing::debug!(
        "opened {} ({} bytes, tail_start={})",
        path.display(),
        size,
        tail_start
    );
    Ok(DualFd {
        head,
        tail,
        size,
        tail_start,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_dual_positions_tail_at_eof() {
        let dir = tempfile_dir();
        let path = dir.join("f.txt");
        std::fs::write(&path, b"hello\nworld\n").unwrap();
        let mut d = open_dual(&path).unwrap();
        assert_eq!(d.size, 12);
        // Tail position should equal size.
        use std::io::Seek;
        let pos = d.tail.stream_position().unwrap();
        assert_eq!(pos, 12);
    }
    fn tempfile_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lr-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
