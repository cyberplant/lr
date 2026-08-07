//! logfmt / `key=value` plugin. Extracts pairs into fields.

use crate::pipeline::parser::{FieldValue, ParsedLine};
use crate::plugin::rust::detect::logfmt_fraction;
use crate::plugin::Plugin;

pub struct Logfmt;

impl Plugin for Logfmt {
    fn name(&self) -> &str {
        "logfmt"
    }

    fn detect(&self, sample: &[&str]) -> f32 {
        let f = logfmt_fraction(sample);
        if f > 0.5 {
            f
        } else {
            0.0
        }
    }

    fn parse(&self, line: &mut ParsedLine) {
        for (k, v) in tokenize(&line.raw) {
            crate::plugin::set_field(line, &k, FieldValue::Str(v));
        }
    }
}

/// A tiny logfmt tokenizer. Handles `key=value`, `key="quoted value"`, and
/// bare `key` (treated as `key=true`).
fn tokenize(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let key = &s[key_start..i];
        if i >= bytes.len() || bytes[i].is_ascii_whitespace() {
            out.push((key.to_string(), "true".to_string()));
            continue;
        }
        // bytes[i] == '='
        i += 1; // skip '='
        let val_start = i;
        if i < bytes.len() && bytes[i] == b'"' {
            i += 1;
            let qs = i;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            out.push((key.to_string(), s[qs..i.min(bytes.len())].to_string()));
            if i < bytes.len() {
                i += 1; // closing quote
            }
        } else {
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            out.push((key.to_string(), s[val_start..i].to_string()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pairs() {
        let mut l = ParsedLine::stub(r#"level=error msg="boom boom" fast"#);
        Logfmt.parse(&mut l);
        assert_eq!(l.fields.get("level"), Some(&FieldValue::Str("error".into())));
        assert_eq!(l.fields.get("msg"), Some(&FieldValue::Str("boom boom".into())));
        assert_eq!(l.fields.get("fast"), Some(&FieldValue::Str("true".into())));
    }
}
