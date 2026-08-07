//! Syslog format plugin. Stub for phase 0; full parser in phase 2.

use crate::pipeline::parser::ParsedLine;
use crate::plugin::Plugin;

pub struct Syslog;

impl Plugin for Syslog {
    fn name(&self) -> &str {
        "syslog"
    }

    fn detect(&self, sample: &[&str]) -> f32 {
        // `<PRI>timestamp host tag: message` — RFC3164/5424.
        let re = regex::Regex::new(r"^<\d{1,3}>").unwrap();
        let hits = sample.iter().filter(|l| re.is_match(l.trim_start())).count();
        let f = hits as f32 / sample.len().max(1) as f32;
        if f > 0.5 {
            f
        } else {
            0.0
        }
    }

    fn parse(&self, _line: &mut ParsedLine) {
        // TODO(phase 2): parse PRI, timestamp, host, tag, message.
    }
}
