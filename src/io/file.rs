//! Opening a file with two file descriptors: a head reader streaming from
//! byte 0, and a tail reader holding the end position and following appends.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};

/// A pair of handles to the same file: one reading forward from the start,
/// one positioned at the end ready to follow appends.
pub struct DualFd {
    pub head: File,
    pub tail: File,
    pub size: u64,
}

/// Open `path` twice and position the head at byte 0 and the tail at EOF.
pub fn open_dual(path: &Path) -> Result<DualFd> {
    let head = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut tail = std::fs::File::open(path)
        .with_context(|| format!("open (tail) {}", path.display()))?;
    let size = head
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    // Seek tail to end.
    use std::io::Seek;
    tail.seek(std::io::SeekFrom::End(0))
        .with_context(|| format!("seek to end {}", path.display()))?;
    tracing::debug!("opened {} ({} bytes)", path.display(), size);
    Ok(DualFd { head, tail, size })
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
