//! SQLite-backed storage. Stores parsed lines and their extracted fields for
//! SQL queries and histogram generation.
//!
//! Inserts are batched: the DB task drains a channel and inserts in
//! transactions of up to 1000 lines at a time.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use crate::pipeline::parser::{FieldValue, ParsedLine};

pub struct SqliteStore {
    conn: Connection,
}

impl SqliteStore {
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open in-memory sqlite")?;
        conn.execute_batch(SCHEMA).context("create schema")?;
        // WAL mode is not available for in-memory DBs, but synchronous=NORMAL
        // still helps by reducing fsync calls (no-op for memory but harmless).
        conn.pragma_update(None, "journal_mode", "memory")?;
        Ok(Self { conn })
    }

    /// Insert a single parsed line and its extracted fields.
    pub fn insert(&self, line: &ParsedLine) -> Result<()> {
        let severity = line.severity.map(|s| format!("{:?}", s));
        let payload = line
            .json
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default());

        self.conn.execute(
            "INSERT INTO lines (ts, ts_ns, severity, source, byte_offset, raw, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                line.timestamp_ns.map(|ns| ns / 1_000_000_000),
                line.timestamp_ns,
                severity,
                line.source,
                line.byte_offset as i64,
                line.raw,
                payload,
            ],
        )?;

        let line_id = self.conn.last_insert_rowid();

        // Insert extracted fields.
        for (key, value) in &line.fields {
            let v_str = match value {
                FieldValue::Str(s) => s.clone(),
                FieldValue::Int(n) => n.to_string(),
                FieldValue::Float(f) => f.to_string(),
                FieldValue::Bool(b) => b.to_string(),
                FieldValue::Null => continue,
            };
            self.conn.execute(
                "INSERT INTO fields (line_id, key, value) VALUES (?1, ?2, ?3)",
                params![line_id, key, v_str],
            )?;
        }

        Ok(())
    }

    /// Insert a batch of parsed lines in a single transaction.
    /// Much faster than individual inserts.
    pub fn insert_batch(&self, lines: &[ParsedLine]) -> Result<()> {
        if lines.is_empty() {
            return Ok(());
        }

        let tx = self.conn.unchecked_transaction()?;
        for line in lines {
            let severity = line.severity.map(|s| format!("{:?}", s));
            let payload = line
                .json
                .as_ref()
                .map(|v| serde_json::to_string(v).unwrap_or_default());

            tx.execute(
                "INSERT INTO lines (ts, ts_ns, severity, source, byte_offset, raw, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    line.timestamp_ns.map(|ns| ns / 1_000_000_000),
                    line.timestamp_ns,
                    severity,
                    line.source,
                    line.byte_offset as i64,
                    line.raw,
                    payload,
                ],
            )?;

            let line_id = tx.last_insert_rowid();

            for (key, value) in &line.fields {
                let v_str = match value {
                    FieldValue::Str(s) => s.clone(),
                    FieldValue::Int(n) => n.to_string(),
                    FieldValue::Float(f) => f.to_string(),
                    FieldValue::Bool(b) => b.to_string(),
                    FieldValue::Null => continue,
                };
                tx.execute(
                    "INSERT INTO fields (line_id, key, value) VALUES (?1, ?2, ?3)",
                    params![line_id, key, v_str],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Execute an arbitrary SQL query and return rows as vectors of
    /// (column_name, value_string) pairs.
    pub fn query(&self, sql: &str) -> Result<Vec<QueryRow>> {
        let mut stmt = self.conn.prepare(sql).context("prepare query")?;
        let col_count = stmt.column_count();
        let col_names: Vec<String> = stmt
            .column_names()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let rows = stmt
            .query_map([], |row| {
                let mut values: Vec<String> = Vec::with_capacity(col_count);
                for i in 0..col_count {
                    let val: rusqlite::Result<String> = row.get::<_, rusqlite::types::Value>(i)
                        .map(|v| match v {
                            rusqlite::types::Value::Null => "NULL".to_string(),
                            rusqlite::types::Value::Integer(n) => n.to_string(),
                            rusqlite::types::Value::Real(f) => f.to_string(),
                            rusqlite::types::Value::Text(s) => s,
                            rusqlite::types::Value::Blob(b) => format!("<blob {} bytes>", b.len()),
                        });
                    match val {
                        Ok(s) => values.push(s),
                        Err(_) => values.push("?".to_string()),
                    }
                }
                Ok(values)
            })
            .context("query_map")?;

        let mut result = Vec::new();
        for row in rows {
            let values = row?;
            result.push(QueryRow {
                columns: col_names.clone(),
                values,
            });
        }
        Ok(result)
    }

    /// Generate a time-bucket histogram of line counts.
    /// Returns (bucket_start_epoch_seconds, count) pairs.
    pub fn histogram(&self, bucket_secs: i64) -> Result<Vec<(i64, i64)>> {
        if bucket_secs <= 0 {
            return Err(anyhow::anyhow!("bucket_secs must be positive"));
        }
        let sql = "SELECT (ts_ns / ?1) * ?1 AS bucket, COUNT(*) as cnt \
             FROM lines \
             WHERE ts_ns IS NOT NULL \
             GROUP BY bucket \
             ORDER BY bucket";
        let bucket_ns = bucket_secs * 1_000_000_000;
        let mut stmt = self.conn.prepare(sql).context("prepare histogram")?;
        let rows = stmt
            .query_map(params![bucket_ns], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .context("query_map histogram")?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Count total lines in the DB.
    pub fn line_count(&self) -> Result<i64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM lines", [], |r| r.get(0))
            .context("count lines")?;
        Ok(n)
    }
}

/// A row returned from a SQL query.
#[derive(Debug, Clone)]
pub struct QueryRow {
    pub columns: Vec<String>,
    pub values: Vec<String>,
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
CREATE INDEX IF NOT EXISTS idx_fields_line ON fields(line_id);
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::Severity;

    #[test]
    fn opens_and_creates_schema() {
        let s = SqliteStore::open_in_memory().unwrap();
        let n: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM lines", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn insert_single_line() {
        let s = SqliteStore::open_in_memory().unwrap();
        let mut line = ParsedLine::stub("error: something broke");
        line.severity = Some(Severity::Error);
        line.source = "test.log".to_string();
        line.byte_offset = 42;
        line.timestamp_ns = Some(1_700_000_000_000_000_000);
        line.fields.insert("level".into(), FieldValue::Str("error".into()));
        line.fields.insert("code".into(), FieldValue::Int(500));

        s.insert(&line).unwrap();

        assert_eq!(s.line_count().unwrap(), 1);

        let rows = s.query("SELECT severity, source, raw FROM lines").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].values[0], "Error");
        assert_eq!(rows[0].values[1], "test.log");
        assert_eq!(rows[0].values[2], "error: something broke");

        // Check fields were inserted.
        let field_count: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM fields", [], |r| r.get(0))
            .unwrap();
        assert_eq!(field_count, 2);
    }

    #[test]
    fn insert_batch() {
        let s = SqliteStore::open_in_memory().unwrap();
        let lines: Vec<ParsedLine> = (0..100)
            .map(|i| {
                let mut l = ParsedLine::stub(&format!("line {i}"));
                l.source = "test".to_string();
                l.byte_offset = i * 10;
                l
            })
            .collect();

        s.insert_batch(&lines).unwrap();
        assert_eq!(s.line_count().unwrap(), 100);
    }

    #[test]
    fn query_selects_fields() {
        let s = SqliteStore::open_in_memory().unwrap();
        let mut line = ParsedLine::stub("test");
        line.fields.insert("user".into(), FieldValue::Str("alice".into()));
        line.fields.insert("status".into(), FieldValue::Int(200));
        s.insert(&line).unwrap();

        let rows = s
            .query("SELECT key, value FROM fields WHERE key = 'user'")
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].values[1], "alice");
    }

    #[test]
    fn histogram_buckets() {
        let s = SqliteStore::open_in_memory().unwrap();
        // Insert lines at t=0, t=5s, t=10s, t=11s
        for ns in [0i64, 5_000_000_000, 10_000_000_000, 11_000_000_000] {
            let mut l = ParsedLine::stub("test");
            l.timestamp_ns = Some(ns);
            s.insert(&l).unwrap();
        }

        // 10-second buckets: bucket 0 has 1 line (t=0), bucket 10 has 2 (t=10,11).
        // But wait, t=5 is in bucket 0 (5/10=0). So bucket 0 has 2 (t=0, t=5).
        let buckets = s.histogram(10).unwrap();
        assert_eq!(buckets.len(), 2);
        // bucket 0 (0-10s): t=0 and t=5 -> 2 lines
        assert_eq!(buckets[0], (0, 2));
        // bucket 10 (10-20s): t=10 and t=11 -> 2 lines
        assert_eq!(buckets[1], (10_000_000_000, 2));
    }

    #[test]
    fn histogram_ignores_null_timestamps() {
        let s = SqliteStore::open_in_memory().unwrap();
        // Line with no timestamp.
        s.insert(&ParsedLine::stub("no ts")).unwrap();
        // Line with timestamp.
        let mut l = ParsedLine::stub("with ts");
        l.timestamp_ns = Some(1_700_000_000_000_000_000);
        s.insert(&l).unwrap();

        let buckets = s.histogram(60).unwrap();
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].1, 1); // only the line with a timestamp
    }

    #[test]
    fn query_count_by_severity() {
        let s = SqliteStore::open_in_memory().unwrap();
        for i in 0..10 {
            let mut l = ParsedLine::stub(&format!("line {i}"));
            l.severity = Some(if i < 3 { Severity::Error } else { Severity::Info });
            s.insert(&l).unwrap();
        }

        let rows = s
            .query("SELECT severity, COUNT(*) as cnt FROM lines WHERE severity IS NOT NULL GROUP BY severity ORDER BY severity")
            .unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].values[0], "Error");
        assert_eq!(rows[0].values[1], "3");
        assert_eq!(rows[1].values[0], "Info");
        assert_eq!(rows[1].values[1], "7");
    }
}
