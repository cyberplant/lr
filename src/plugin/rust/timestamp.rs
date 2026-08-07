//! Timestamp normalization. Detects common formats and converts to epoch
//! nanoseconds. Stub-grade for phase 0; full coverage in phase 2.

use crate::pipeline::parser::ParsedLine;
use crate::plugin::Plugin;

pub struct Timestamp;

impl Plugin for Timestamp {
    fn name(&self) -> &str {
        "timestamp"
    }

    fn detect(&self, _sample: &[&str]) -> f32 {
        0.1
    }

    fn parse(&self, line: &mut ParsedLine) {
        if line.timestamp_ns.is_some() {
            return;
        }
        // Honor a `ts`/`time`/`timestamp` field set by an earlier plugin.
        if let Some(crate::pipeline::parser::FieldValue::Str(s)) =
            line.fields.get("ts").or_else(|| line.fields.get("time")).or_else(|| line.fields.get("timestamp"))
            && let Some(ns) = parse_one(s)
        {
            line.timestamp_ns = Some(ns);
            return;
        }
        // Otherwise try the start of the raw line.
        if let Some(ns) = parse_one(line.raw.trim_start()) {
            line.timestamp_ns = Some(ns);
        }
    }
}

/// Try to parse a timestamp from the start of `s`. Returns epoch nanoseconds
/// on success. Supports a small set of formats for now.
pub fn parse_one(s: &str) -> Option<i64> {
    // Pure epoch seconds / millis / micros / nanos.
    if let Some(ns) = parse_epoch(s) {
        return Some(ns);
    }
    // ISO8601 / RFC3339.
    if let Some(ns) = parse_iso(s) {
        return Some(ns);
    }
    None
}

fn parse_epoch(s: &str) -> Option<i64> {
    let end = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());
    let tok = &s[..end];
    if tok.is_empty() {
        return None;
    }
    let v: f64 = tok.parse().ok()?;
    // Heuristic: choose unit by magnitude.
    let ns = if v >= 1e18 {
        v // already ns
    } else if v >= 1e15 {
        v * 1e3 // micros
    } else if v >= 1e12 {
        v * 1e6 // millis
    } else if v >= 1e9 {
        v * 1e9 // seconds
    } else {
        // Treat small numbers as seconds too (could be a relative ts).
        v * 1e9
    };
    Some(ns as i64)
}

fn parse_iso(_s: &str) -> Option<i64> {
    // TODO(phase 2): ISO8601 / RFC3339 / syslog date parsing via `chrono` or
    // `time`. Deferred to keep phase 0 deps minimal.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_epoch_seconds() {
        assert!(parse_one("1700000000 ").is_some());
    }

    #[test]
    fn parses_epoch_nanos() {
        let ns = parse_one("1700000000000000000").unwrap();
        assert!(ns > 1e18 as i64);
    }
}
