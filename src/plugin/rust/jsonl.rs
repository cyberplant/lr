//! JSONL plugin: parse each line as a JSON object, extract fields, and stash
//! the value for pretty-printing. Syntax coloring happens in the UI layer
//! using the stashed `serde_json::Value`.

use crate::pipeline::parser::{FieldValue, ParsedLine};
use crate::plugin::rust::detect::json_object_fraction;
use crate::plugin::Plugin;

pub struct Jsonl;

impl Plugin for Jsonl {
    fn name(&self) -> &str {
        "jsonl"
    }

    fn detect(&self, sample: &[&str]) -> f32 {
        let f = json_object_fraction(sample);
        if f > 0.8 {
            f
        } else {
            0.0
        }
    }

    fn parse(&self, line: &mut ParsedLine) {
        let trimmed = line.raw.trim_start();
        if !trimmed.starts_with('{') {
            return;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            return;
        };
        if let Some(obj) = v.as_object() {
            for (k, val) in obj {
                set_field_from_json(line, k, val);
            }
        }
        line.json = Some(v);
    }
}

fn set_field_from_json(line: &mut ParsedLine, key: &str, val: &serde_json::Value) {
    let fv = match val {
        serde_json::Value::Null => FieldValue::Null,
        serde_json::Value::Bool(b) => FieldValue::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                FieldValue::Int(i)
            } else if let Some(f) = n.as_f64() {
                FieldValue::Float(f)
            } else {
                return;
            }
        }
        serde_json::Value::String(s) => FieldValue::Str(s.clone()),
        // Nested values are not flattened into fields in v1; they remain in
        // `line.json` for pretty-print and JSONPath queries.
        _ => return,
    };
    crate::plugin::set_field(line, key, fv);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_jsonl() {
        let mut l = ParsedLine::stub(r#"{"level":"error","msg":"boom","n":3}"#);
        Jsonl.parse(&mut l);
        assert_eq!(l.fields.get("level"), Some(&FieldValue::Str("error".into())));
        assert_eq!(l.fields.get("n"), Some(&FieldValue::Int(3)));
        assert!(l.json.is_some());
    }

    #[test]
    fn detect_high_confidence_on_json() {
        let s = vec![r#"{"a":1}"#, r#"{"b":2}"#];
        assert!(Jsonl.detect(&s) > 0.8);
    }
}
