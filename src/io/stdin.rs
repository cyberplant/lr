//! Stdin streaming source. Reads stdin line-by-line and sends raw lines on
//! a tokio channel.

use std::io::BufRead;

use anyhow::Result;
use tokio::sync::mpsc::Sender;

use crate::io::RawLine;

/// Read from stdin line-by-line and send each line on `tx`.
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
