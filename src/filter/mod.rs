//! Filter expressions: AST with AND/OR, severity comparisons, field/JSONPath
//! predicates, and regex sub-matches. Stub for phase 0; impl in phase 6.

use crate::pipeline::parser::ParsedLine;
use crate::plugin::Severity;

/// A filter expression tree.
#[derive(Debug, Clone)]
pub enum Filter {
    All(Vec<Filter>),
    Any(Vec<Filter>),
    Not(Box<Filter>),
    Severity(Severity),
    FieldEq { key: String, value: String },
    Regex(regex::Regex),
}

impl Filter {
    /// Evaluate the filter against a parsed line.
    pub fn matches(&self, line: &ParsedLine) -> bool {
        match self {
            Filter::All(parts) => parts.iter().all(|p| p.matches(line)),
            Filter::Any(parts) => parts.iter().any(|p| p.matches(line)),
            Filter::Not(inner) => !inner.matches(line),
            Filter::Severity(want) => line.severity == Some(*want),
            Filter::FieldEq { key, value } => line
                .fields
                .get(key)
                .and_then(|fv| match fv {
                    crate::pipeline::parser::FieldValue::Str(s) => Some(s.as_str()),
                    _ => None,
                })
                .map(|s| s == value.as_str())
                .unwrap_or(false),
            Filter::Regex(re) => re.is_match(&line.raw),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::parser::FieldValue;

    #[test]
    fn severity_filter_matches() {
        let mut l = ParsedLine::stub("x");
        l.severity = Some(Severity::Error);
        assert!(Filter::Severity(Severity::Error).matches(&l));
        assert!(!Filter::Severity(Severity::Info).matches(&l));
    }

    #[test]
    fn field_eq_filter_matches() {
        let mut l = ParsedLine::stub("x");
        l.fields.insert("k".into(), FieldValue::Str("v".into()));
        assert!(Filter::FieldEq { key: "k".into(), value: "v".into() }.matches(&l));
    }

    #[test]
    fn and_combinator() {
        let mut l = ParsedLine::stub("error: boom");
        l.severity = Some(Severity::Error);
        let f = Filter::All(vec![
            Filter::Severity(Severity::Error),
            Filter::Regex(regex::Regex::new("boom").unwrap()),
        ]);
        assert!(f.matches(&l));
    }
}
