//! Common Log Format / NCSA plugin. Stub for phase 0.

use crate::pipeline::parser::ParsedLine;
use crate::plugin::Plugin;

pub struct Clf;

impl Plugin for Clf {
    fn name(&self) -> &str {
        "clf"
    }

    fn detect(&self, sample: &[&str]) -> f32 {
        // `host ident authuser [date] "request" status bytes`
        let re = regex::Regex::new(r#"^\S+ \S+ \S+ \[.+?\] ".+?" \d{3} "#).unwrap();
        let hits = sample.iter().filter(|l| re.is_match(l.trim_start())).count();
        let f = hits as f32 / sample.len().max(1) as f32;
        if f > 0.5 {
            f
        } else {
            0.0
        }
    }

    fn parse(&self, _line: &mut ParsedLine) {
        // TODO(phase 2): extract method, path, status, bytes, duration.
    }
}
