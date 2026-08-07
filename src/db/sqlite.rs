//! SQLite-backed storage. Stub schema for phase 0; full impl in phase 4.

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::pipeline::parser::ParsedLine;

pub struct SqliteStore {
    conn: Connection,
}

impl SqliteStore {
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open in-memory sqlite")?;
        conn.execute_batch(SCHEMA).context("create schema")?;
        Ok(Self { conn })
    }

    pub fn insert(&self, _line: &ParsedLine) -> Result<()> {
        // TODO(phase 4): batched prepared-statement insert.
        Ok(())
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS lines (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts          INTEGER,
    ts_ns       INTEGER,
    severity    TEXT,
    source      TEXT,
    byte_offset INTEGER,
    raw         TEXT,
    payload     TEXT
);
CREATE TABLE IF NOT EXISTS fields (
    line_id INTEGER NOT NULL,
    key     TEXT    NOT NULL,
    value   TEXT
);
CREATE INDEX IF NOT EXISTS idx_lines_ts ON lines(ts_ns);
CREATE INDEX IF NOT EXISTS idx_fields_kv ON fields(key, value);
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_and_creates_schema() {
        let s = SqliteStore::open_in_memory().unwrap();
        // Schema should exist: count from lines should be 0.
        let n: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM lines", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }
}
