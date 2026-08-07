//! User-supplied regex with named captures → fields. Stub for phase 0; the
//! regex is configured per-source in `config.toml` or via `--plugin`.

use crate::pipeline::parser::{FieldValue, ParsedLine};
use crate::plugin::Plugin;

pub struct RegexCapture {
    name: String,
    re: regex::Regex,
}

impl RegexCapture {
    pub fn new(name: String, pattern: &str) -> Result<Self, regex::Error> {
        Ok(Self {
            name,
            re: regex::Regex::new(pattern)?,
        })
    }
}

impl Plugin for RegexCapture {
    fn name(&self) -> &str {
        &self.name
    }

    fn detect(&self, sample: &[&str]) -> f32 {
        let hits = sample.iter().filter(|l| self.re.is_match(l)).count();
        let f = hits as f32 / sample.len().max(1) as f32;
        if f > 0.5 {
            f
        } else {
            0.0
        }
    }

    fn parse(&self, line: &mut ParsedLine) {
        let Some(caps) = self.re.captures(&line.raw) else {
            return;
        };
        let names: Vec<(String, String)> = self
            .re
            .capture_names()
            .flatten()
            .filter_map(|name| caps.name(name).map(|m| (name.to_string(), m.as_str().to_string())))
            .collect();
        for (name, value) in names {
            crate::plugin::set_field(line, &name, FieldValue::Str(value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_named_groups() {
        let p = RegexCapture::new("test".into(), r"(?P<level>\w+): (?P<msg>.+)").unwrap();
        let mut l = ParsedLine::stub("error: boom");
        p.parse(&mut l);
        assert_eq!(l.fields.get("level"), Some(&FieldValue::Str("error".into())));
        assert_eq!(l.fields.get("msg"), Some(&FieldValue::Str("boom".into())));
    }
}
