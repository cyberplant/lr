//! Severity detection. Runs as a core plugin that inspects the raw line (and
//! any `level`/`severity` field populated by an earlier plugin) and sets
//! `line.severity`.

use crate::pipeline::parser::ParsedLine;
use crate::plugin::{Plugin, Severity};

pub struct SeverityDetector;

impl Plugin for SeverityDetector {
    fn name(&self) -> &str {
        "severity"
    }

    fn detect(&self, _sample: &[&str]) -> f32 {
        // Always run; it's a no-op if nothing matches.
        0.1
    }

    fn parse(&self, line: &mut ParsedLine) {
        if line.severity.is_some() {
            return;
        }
        // First, honor an explicit field set by an earlier plugin.
        if let Some(crate::pipeline::parser::FieldValue::Str(s)) = line.fields.get("level")
            && let Some(sev) = from_word(s)
        {
            line.severity = Some(sev);
            return;
        }
        // Otherwise scan the raw text for a level word.
        let raw_lower = line.raw.to_ascii_lowercase();
        line.severity = scan_word(&raw_lower);
    }
}

fn from_word(s: &str) -> Option<Severity> {
    match s.to_ascii_lowercase().as_str() {
        "trace" => Some(Severity::Trace),
        "debug" => Some(Severity::Debug),
        "info" | "information" | "notice" => Some(Severity::Info),
        "warn" | "warning" => Some(Severity::Warn),
        "error" | "err" | "fatal" | "critical" | "crit" | "alert" | "emerg" => {
            Some(Severity::Error)
        }
        _ => None,
    }
}

fn scan_word(lower: &str) -> Option<Severity> {
    // Order matters: check Error before Info so "error" isn't shadowed.
    for word in ["error", "err", "fatal", "critical", "crit", "alert", "emerg"] {
        if contains_word(lower, word) {
            return Some(Severity::Error);
        }
    }
    for word in ["warn", "warning"] {
        if contains_word(lower, word) {
            return Some(Severity::Warn);
        }
    }
    for word in ["debug"] {
        if contains_word(lower, word) {
            return Some(Severity::Debug);
        }
    }
    for word in ["trace"] {
        if contains_word(lower, word) {
            return Some(Severity::Trace);
        }
    }
    for word in ["info", "information", "notice"] {
        if contains_word(lower, word) {
            return Some(Severity::Info);
        }
    }
    None
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    // Cheap word-boundary check: surrounded by non-alphanumeric or string ends.
    let mut start = 0;
    let bytes = haystack.as_bytes();
    while start + needle.len() <= bytes.len() {
        if let Some(idx) = haystack[start..].find(needle) {
            let abs = start + idx;
            let before_ok = abs == 0 || !bytes[abs - 1].is_ascii_alphanumeric();
            let after_idx = abs + needle.len();
            let after_ok = after_idx >= bytes.len() || !bytes[after_idx].is_ascii_alphanumeric();
            if before_ok && after_ok {
                return true;
            }
            start = abs + 1;
        } else {
            break;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_error_word() {
        let mut l = ParsedLine::stub("something error happened");
        SeverityDetector.parse(&mut l);
        assert_eq!(l.severity, Some(Severity::Error));
    }

    #[test]
    fn honors_level_field() {
        let mut l = ParsedLine::stub("x");
        l.fields.insert(
            "level".into(),
            crate::pipeline::parser::FieldValue::Str("WARN".into()),
        );
        SeverityDetector.parse(&mut l);
        assert_eq!(l.severity, Some(Severity::Warn));
    }

    #[test]
    fn no_false_positive_in_words() {
        // "informational" should not match "info"? It contains "info" but with
        // an alphanumeric after, so word-boundary check rejects it.
        let mut l = ParsedLine::stub("informational only");
        SeverityDetector.parse(&mut l);
        assert_eq!(l.severity, None);
    }
}
