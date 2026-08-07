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
        // Also check a "severity" field.
        if let Some(crate::pipeline::parser::FieldValue::Str(s)) = line.fields.get("severity")
            && let Some(sev) = from_word(s)
        {
            line.severity = Some(sev);
            return;
        }
        // Check for explicit bracketed level like [DEBUG], [INFO], [ERROR].
        let raw_lower = line.raw.to_ascii_lowercase();
        if let Some(sev) = scan_bracketed(&raw_lower) {
            line.severity = Some(sev);
            return;
        }
        // Fall back to scanning for level words.
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

/// Scan for explicit bracketed level indicators like `[DEBUG]`, `[INFO]`.
/// These are very common in logging frameworks (Python logging, Java log4j,
/// etc.) and are high-confidence signals — much more reliable than word
/// scanning. We check these before generic word matching so that a line like
/// `[DEBUG] ... err=(0.31,0.00)` is correctly detected as Debug, not Error.
fn scan_bracketed(lower: &str) -> Option<Severity> {
    // Look for [LEVEL] patterns. Check in specificity order: longer/more
    // explicit words first to avoid e.g. [WARN] matching before [WARNING].
    let patterns: &[(&str, Severity)] = &[
        ("[critical]", Severity::Error),
        ("[warning]", Severity::Warn),
        ("[information]", Severity::Info),
        ("[trace]", Severity::Trace),
        ("[debug]", Severity::Debug),
        ("[error]", Severity::Error),
        ("[fatal]", Severity::Error),
        ("[alert]", Severity::Error),
        ("[emerg]", Severity::Error),
        ("[warn]", Severity::Warn),
        ("[info]", Severity::Info),
        ("[notice]", Severity::Info),
        ("[crit]", Severity::Error),
    ];
    for (pat, sev) in patterns {
        if lower.contains(pat) {
            return Some(*sev);
        }
    }
    None
}

fn scan_word(lower: &str) -> Option<Severity> {
    // Order matters: check Error before Info so "error" isn't shadowed.
    // Note: "err" is intentionally excluded from generic scanning — it's
    // too short and causes false positives (e.g. "err=(0.31,0.00)" in a
    // DEBUG line). Use bracketed detection or level= field for "err".
    for word in ["error", "fatal", "critical", "crit", "alert", "emerg"] {
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

    #[test]
    fn bracketed_debug_overrides_err_substring() {
        // The exact case from the user: [DEBUG] line with err= in the content
        let mut l = ParsedLine::stub(
            "2026-07-31 01:50:26,048 [DEBUG] robot.navigation.engine: walk_bias updated: err=(-0.31,0.00) bias=(-0.45,-0.11)",
        );
        SeverityDetector.parse(&mut l);
        assert_eq!(l.severity, Some(Severity::Debug));
    }

    #[test]
    fn bracketed_levels_detected() {
        let cases = [
            ("[ERROR] something broke", Severity::Error),
            ("[WARN] careful", Severity::Warn),
            ("[WARNING] careful", Severity::Warn),
            ("[INFO] ok", Severity::Info),
            ("[DEBUG] details", Severity::Debug),
            ("[TRACE] verbose", Severity::Trace),
            ("[FATAL] dead", Severity::Error),
            ("[CRITICAL] bad", Severity::Error),
        ];
        for (raw, expected) in cases {
            let mut l = ParsedLine::stub(raw);
            SeverityDetector.parse(&mut l);
            assert_eq!(l.severity, Some(expected), "failed for: {raw}");
        }
    }

    #[test]
    fn err_word_alone_not_detected() {
        // "err" alone should NOT trigger Error (too many false positives).
        // Only "error" or explicit [ERROR] / level=err should.
        let mut l = ParsedLine::stub("walk_bias updated: err=(-0.31,0.00)");
        SeverityDetector.parse(&mut l);
        assert_eq!(l.severity, None);
    }

    #[test]
    fn honors_severity_field() {
        let mut l = ParsedLine::stub("x");
        l.fields.insert(
            "severity".into(),
            crate::pipeline::parser::FieldValue::Str("DEBUG".into()),
        );
        SeverityDetector.parse(&mut l);
        assert_eq!(l.severity, Some(Severity::Debug));
    }
}
