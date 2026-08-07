//! ANSI escape sequence handling. Strips or passes through SGR codes based
//! on config. Stub for phase 0.

use crate::pipeline::parser::ParsedLine;
use crate::plugin::Plugin;

pub struct Ansi {
    pub strip: bool,
}

impl Plugin for Ansi {
    fn name(&self) -> &str {
        "ansi"
    }

    fn detect(&self, _sample: &[&str]) -> f32 {
        0.1
    }

    fn parse(&self, line: &mut ParsedLine) {
        if self.strip && line.raw.contains('\u{1b}') {
            line.raw = strip_ansi(&line.raw);
        }
    }
}

/// Remove ANSI escape sequences from `s`.
pub fn strip_ansi(s: &str) -> String {
    let re = regex::Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").unwrap();
    re.replace_all(s, "").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_codes() {
        let s = "\x1b[31mred\x1b[0m";
        assert_eq!(strip_ansi(s), "red");
    }
}
