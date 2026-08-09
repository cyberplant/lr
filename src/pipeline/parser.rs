//! The parser pipeline. Each raw line is run through the active plugin chain
//! to produce a `ParsedLine`.

use std::collections::HashMap;

use crate::plugin::Plugin;

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

impl FieldValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            FieldValue::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            FieldValue::Int(n) => Some(*n),
            _ => None,
        }
    }
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

/// The parser pipeline. Holds a plugin registry for format detection and
/// always-on plugins (timestamp, severity). Each raw line is run through the
/// detected format plugin, then timestamp normalization, then severity
/// detection.
pub struct Parser {
    registry: crate::plugin::Registry,
    timestamp: crate::plugin::rust::timestamp::Timestamp,
    severity: crate::plugin::rust::severity::SeverityDetector,
    detected_format: Option<String>,
    sample: Vec<String>,
    line_no: u64,
}

/// Maximum lines to accumulate for format detection before giving up.
const DETECT_SAMPLE_MAX: usize = 64;

impl Parser {
    pub fn new() -> Self {
        Self {
            registry: crate::plugin::build_default_registry(),
            timestamp: crate::plugin::rust::timestamp::Timestamp,
            severity: crate::plugin::rust::severity::SeverityDetector,
            detected_format: None,
            sample: Vec::with_capacity(DETECT_SAMPLE_MAX),
            line_no: 0,
        }
    }

    /// Parse a raw line into a `ParsedLine` by running the plugin chain.
    pub fn parse(&mut self, raw: crate::io::RawLine) -> ParsedLine {
        self.line_no += 1;
        let mut line = ParsedLine {
            line_no: self.line_no,
            byte_offset: raw.byte_offset,
            raw: raw.raw.clone(),
            timestamp_ns: None,
            severity: None,
            source: raw.source,
            fields: HashMap::new(),
            json: None,
        };

        // Try to detect format from accumulated sample.
        if self.detected_format.is_none() && self.sample.len() < DETECT_SAMPLE_MAX {
            self.sample.push(raw.raw);
            let refs: Vec<&str> = self.sample.iter().map(|s| s.as_str()).collect();
            if let Some(plugin) = self.registry.detect_best(&refs) {
                self.detected_format = Some(plugin.name().to_string());
                tracing::info!("detected format: {}", plugin.name());
            }
        }

        // Run format plugin if detected.
        if let Some(name) = &self.detected_format
            && let Some(plugin) = self.registry.get_by_name(name)
        {
            plugin.parse(&mut line);
        }

        // Always run timestamp and severity.
        self.timestamp.parse(&mut line);
        self.severity.parse(&mut line);

        line
    }
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::RawLine;

    #[test]
    fn stub_line_is_empty() {
        let l = ParsedLine::stub("x");
        assert_eq!(l.raw, "x");
        assert!(l.timestamp_ns.is_none());
        assert!(l.fields.is_empty());
    }

    #[test]
    fn parser_detects_jsonl_and_extracts_fields() {
        let mut p = Parser::new();
        let raw = RawLine {
            source: "test".into(),
            byte_offset: 0,
            raw: r#"{"level":"error","msg":"boom"}"#.into(),
        };
        let line = p.parse(raw);
        assert_eq!(p.detected_format.as_deref(), Some("jsonl"));
        assert_eq!(
            line.fields.get("level"),
            Some(&FieldValue::Str("error".into()))
        );
        assert_eq!(line.severity, Some(crate::plugin::Severity::Error));
    }

    #[test]
    fn parser_detects_logfmt() {
        let mut p = Parser::new();
        let raw = RawLine {
            source: "test".into(),
            byte_offset: 0,
            raw: "level=info msg=hello".into(),
        };
        let line = p.parse(raw);
        assert_eq!(p.detected_format.as_deref(), Some("logfmt"));
        assert_eq!(
            line.fields.get("level"),
            Some(&FieldValue::Str("info".into()))
        );
        assert_eq!(line.severity, Some(crate::plugin::Severity::Info));
    }

    #[test]
    fn parser_assigns_incrementing_line_numbers() {
        let mut p = Parser::new();
        let l1 = p.parse(RawLine {
            source: "t".into(),
            byte_offset: 0,
            raw: "a".into(),
        });
        let l2 = p.parse(RawLine {
            source: "t".into(),
            byte_offset: 2,
            raw: "b".into(),
        });
        assert_eq!(l1.line_no, 1);
        assert_eq!(l2.line_no, 2);
    }
}
