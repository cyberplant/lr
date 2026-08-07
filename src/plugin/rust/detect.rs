//! Format auto-detection over a sample of the first lines.

/// Default number of sample lines used for detection.
pub const SAMPLE_LINES: usize = 64;

/// Heuristic: what fraction of `sample` lines parse as JSON objects?
pub fn json_object_fraction(sample: &[&str]) -> f32 {
    if sample.is_empty() {
        return 0.0;
    }
    let n = sample.len();
    let hits = sample
        .iter()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with('{') && serde_json::from_str::<serde_json::Value>(t).is_ok()
        })
        .count();
    hits as f32 / n as f32
}

/// Heuristic: what fraction of lines look like `key=value` logfmt?
pub fn logfmt_fraction(sample: &[&str]) -> f32 {
    if sample.is_empty() {
        return 0.0;
    }
    let re = regex::Regex::new(r"^[A-Za-z_][\w.-]*=\S").unwrap();
    let hits = sample.iter().filter(|l| re.is_match(l.trim())).count();
    hits as f32 / sample.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_jsonl() {
        let s = vec![r#"{"a":1}"#, r#"{"b":2}"#];
        assert!(json_object_fraction(&s) > 0.99);
    }

    #[test]
    fn detects_logfmt() {
        let s = vec!["k=v a=b", "x=y"];
        assert!(logfmt_fraction(&s) > 0.99);
    }
}
