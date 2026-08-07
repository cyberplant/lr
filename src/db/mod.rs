//! In-memory database for querying fields and rendering histograms.
//!
//! See `PLAN.md` phase 4. Backed by SQLite via `rusqlite` (bundled). Stub for
//! phase 0.

pub mod sqlite;

use anyhow::Result;

/// Abstraction over the storage backend so DuckDB could be swapped in later.
pub trait Storage: Send {
    fn insert(&mut self, line: &crate::pipeline::parser::ParsedLine) -> Result<()>;
    fn query(&self, sql: &str) -> Result<Vec<crate::pipeline::parser::FieldValue>>;
    fn histogram(&self, bucket_secs: i64) -> Result<Vec<(i64, i64)>>;
}
