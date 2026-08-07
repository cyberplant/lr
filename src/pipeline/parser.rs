//! The parser pipeline. Each raw line is run through the active plugin chain
//! to produce a `ParsedLine`.

use std::collections::HashMap;

/// A single parsed log line, the unit the UI and DB operate on.
#[derive(Debug, Clone)]
pub struct ParsedLine {
    /// 1-based line number within the source.
    pub line_no: u64,
    /// Byte offset of the start of the line in the source file.
    pub byte_offset: u64,
    /// Raw line bytes (no trailing newline).
    pub raw: String,
    /// Normalized timestamp in epoch nanoseconds, if detected.
    pub timestamp_ns: Option<i64>,
    /// Detected severity level, if any.
    pub severity: Option<crate::plugin::Severity>,
    /// Source tag (file index or "stdin").
    pub source: String,
    /// Extracted structured fields (e.g. JSON keys, logfmt pairs, regex named
    /// captures).
    pub fields: HashMap<String, FieldValue>,
    /// Parsed JSON value when the line is JSONL, for pretty-printing.
    pub json: Option<serde_json::Value>,
}

/// A field value extracted from a line. Stored in the DB and queryable.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(untagged)]
pub enum FieldValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

impl ParsedLine {
    /// Construct a minimal stub line for tests.
    pub fn stub(raw: &str) -> Self {
        Self {
            line_no: 0,
            byte_offset: 0,
            raw: raw.to_string(),
            timestamp_ns: None,
            severity: None,
            source: String::new(),
            fields: HashMap::new(),
            json: None,
        }
    }
}

// TODO(phase 2): Parser struct that holds the active plugin chain and exposes
// `parse(raw_line) -> ParsedLine`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_line_is_empty() {
        let l = ParsedLine::stub("x");
        assert_eq!(l.raw, "x");
        assert!(l.timestamp_ns.is_none());
        assert!(l.fields.is_empty());
    }
}
