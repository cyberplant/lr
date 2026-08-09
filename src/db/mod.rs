//! In-memory database for querying fields and rendering histograms.
//!
//! Backed by SQLite via `rusqlite` (bundled). The DB runs on a dedicated
//! blocking task that drains a channel of parsed lines and batch-inserts
//! them.

pub mod sqlite;

pub use sqlite::{QueryRow, SqliteStore};

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Shared database handle. Wrapped in a Mutex because rusqlite::Connection
/// is not Sync.
pub type SharedDb = Arc<Mutex<SqliteStore>>;

/// Create a new shared in-memory database.
pub fn create_shared() -> Result<SharedDb> {
    let store = SqliteStore::open_in_memory()?;
    Ok(Arc::new(Mutex::new(store)))
}

/// Maximum lines per batch insert.
const BATCH_SIZE: usize = 1000;

/// Spawn a DB writer task that drains parsed lines from a channel and
/// batch-inserts them into SQLite. Runs on a spawn_blocking thread.
pub fn spawn_db_writer(db: SharedDb, mut rx: tokio::sync::mpsc::Receiver<crate::pipeline::parser::ParsedLine>) {
    tokio::task::spawn_blocking(move || {
        let rt = match tokio::runtime::Handle::try_current() {
            Ok(h) => h,
            Err(_) => {
                tracing::error!("db_writer: no tokio runtime");
                return;
            }
        };
        let mut batch: Vec<crate::pipeline::parser::ParsedLine> = Vec::with_capacity(BATCH_SIZE);

        loop {
            // Try to receive a line. Use blocking_recv in the context of
            // spawn_blocking via a block_on.
            let line = rt.block_on(rx.recv());
            match line {
                Some(l) => {
                    batch.push(l);
                    // Drain any additional lines that are immediately available.
                    while batch.len() < BATCH_SIZE {
                        match rx.try_recv() {
                            Ok(l) => batch.push(l),
                            Err(_) => break,
                        }
                    }
                    // Insert the batch.
                    if let Err(e) = insert_batch(&db, &batch) {
                        tracing::error!("db_writer: insert batch: {e:#}");
                    }
                    batch.clear();
                }
                None => {
                    // Channel closed — flush remaining and exit.
                    if !batch.is_empty()
                        && let Err(e) = insert_batch(&db, &batch)
                    {
                        tracing::error!("db_writer: flush: {e:#}");
                    }
                    tracing::debug!("db_writer: channel closed, exiting");
                    return;
                }
            }
        }
    });
}

fn insert_batch(db: &SharedDb, batch: &[crate::pipeline::parser::ParsedLine]) -> Result<()> {
    // block_on the mutex lock since we're in a blocking thread.
    let rt = tokio::runtime::Handle::try_current();
    match rt {
        Ok(handle) => {
            let store = handle.block_on(db.lock());
            store.insert_batch(batch)
        }
        Err(_) => {
            // Fallback: try_blocking_lock if available, otherwise error.
            Err(anyhow::anyhow!("db_writer: no runtime to lock mutex"))
        }
    }
}
