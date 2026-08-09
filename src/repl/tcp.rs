//! TCP command server. Accepts connections, reads commands line-by-line,
//! and dispatches them against the shared state.

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use crate::app::state::AppState;
use crate::io::RawLine;
use crate::pipeline::index::ReadProgress;
use crate::repl::command::parse;
use crate::repl::dispatcher::{dispatch, OutputFormat, ReplState};

/// Start the TCP command server on `addr`. Accepts connections in a loop;
/// each connection is handled in its own tokio task.
pub async fn run(
    addr: &str,
    state: Arc<Mutex<AppState>>,
    raw_tx: tokio::sync::mpsc::Sender<RawLine>,
    progress: ReadProgress,
    db: Option<crate::db::SharedDb>,
    format: OutputFormat,
) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    tracing::info!("TCP command server listening on {addr}");

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("accept: {e}");
                continue;
            }
        };
        tracing::debug!("TCP connection from {peer}");

        let repl = ReplState {
            state: state.clone(),
            raw_tx: raw_tx.clone(),
            progress: progress.clone(),
            db: db.clone(),
        };
        tokio::spawn(handle_connection(stream, repl, format));
    }
}

async fn handle_connection(stream: TcpStream, repl: ReplState, format: OutputFormat) {
    let peer = stream.peer_addr().ok();
    if let Err(e) = handle_connection_inner(stream, &repl, format).await {
        tracing::warn!("TCP connection {:?}: {e}", peer);
    }
    tracing::debug!("TCP connection {:?} closed", peer);
}

async fn handle_connection_inner(
    stream: TcpStream,
    repl: &ReplState,
    format: OutputFormat,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    // Send a greeting.
    let greeting = match format {
        OutputFormat::Text => "lr command server ready. Type 'help' for commands.\n",
        OutputFormat::Json => r#"{"ok":true,"ready":true}"#,
    };
    writer.write_all(greeting.as_bytes()).await?;
    writer.write_all(b"\n").await?;

    while let Ok(Some(line)) = lines.next_line().await {
        let cmd = parse(&line);
        let result = dispatch(cmd, repl, format).await;
        if !result.output.is_empty() {
            writer.write_all(result.output.as_bytes()).await?;
            writer.write_all(b"\n").await?; // delimiter between responses
            writer.flush().await?;
        }
        if result.quit {
            break;
        }
    }

    Ok(())
}
