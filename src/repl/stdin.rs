//! Stdin REPL transport. Reads commands from stdin line-by-line, dispatches
//! them, and prints responses to stdout.

use std::io::{BufRead, Write};

use anyhow::Result;

use crate::repl::command::parse;
use crate::repl::dispatcher::{dispatch, OutputFormat, ReplState};

/// Run the stdin REPL loop. Reads lines from stdin, dispatches commands,
/// prints responses to stdout. Exits when `quit` is received or stdin closes.
pub async fn run(repl: ReplState, format: OutputFormat) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let reader = std::io::BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    for line in reader.lines() {
        let line = line?;
        let cmd = parse(&line);
        let result = dispatch(cmd, &repl, format).await;
        if !result.output.is_empty() {
            writer.write_all(result.output.as_bytes())?;
            writer.flush()?;
        }
        if result.quit {
            break;
        }
    }

    Ok(())
}
