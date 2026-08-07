//! Search: regex + literal + case-insensitive, match highlighting, next/prev.
//! Stub for phase 0; impl in phase 6.

use regex::Regex;

pub struct Search {
    re: Regex,
}

impl Search {
    pub fn new(pattern: &str, case_insensitive: bool, literal: bool) -> Result<Self, regex::Error> {
        let pat = if literal {
            regex::escape(pattern)
        } else {
            pattern.to_string()
        };
        let mut b = regex::RegexBuilder::new(&pat);
        if case_insensitive {
            b.case_insensitive(true);
        }
        Ok(Self { re: b.build()? })
    }

    pub fn is_match(&self, s: &str) -> bool {
        self.re.is_match(s)
    }

    /// Find all match positions (start, end) in the given text.
    pub fn find_iter(&self, s: &str) -> Vec<(usize, usize)> {
        self.re
            .find_iter(s)
            .map(|m| (m.start(), m.end()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_match() {
        let s = Search::new("err", false, false).unwrap();
        assert!(s.is_match("an error"));
    }

    #[test]
    fn literal_escapes() {
        let s = Search::new("a.b", false, true).unwrap();
        assert!(s.is_match("a.b"));
        assert!(!s.is_match("axb"));
    }
}
