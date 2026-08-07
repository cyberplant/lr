//! Plugin system: Rust core plugins (trait objects) + Lua user plugins.
//!
//! See `PLAN.md` phases 2-3.

pub mod lua;
pub mod rust;

use crate::pipeline::parser::{FieldValue, ParsedLine};

/// A log severity level, in increasing order of urgency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// The trait every plugin implements. Rust core plugins implement this
/// directly; Lua plugins are adapted into this trait by the Lua host.
pub trait Plugin: Send + Sync {
    /// Name used in config and `--plugin`.
    fn name(&self) -> &str;

    /// Return a confidence score in `[0.0, 1.0]` that this plugin can parse
    /// the given sample lines. Higher = more confident.
    fn detect(&self, sample: &[&str]) -> f32;

    /// Mutate a `ParsedLine` in place to fill in fields, severity, timestamp,
    /// JSON, etc. Called in chain order after detection picks the plugin set.
    fn parse(&self, line: &mut ParsedLine);
}

/// Registry of available plugins, used to pick the active set per source.
pub struct Registry {
    plugins: Vec<Box<dyn Plugin>>,
}

impl Registry {
    pub fn new() -> Self {
        Self { plugins: Vec::new() }
    }

    pub fn register(&mut self, plugin: Box<dyn Plugin>) {
        self.plugins.push(plugin);
    }

    pub fn detect_best(&self, sample: &[&str]) -> Option<&dyn Plugin> {
        let mut best: Option<(f32, &dyn Plugin)> = None;
        for p in &self.plugins {
            let score = p.detect(sample);
            if score > 0.0 && best.is_none_or(|(bs, _)| score > bs) {
                best = Some((score, p.as_ref()));
            }
        }
        best.map(|(_, p)| p)
    }

    pub fn get_by_name(&self, name: &str) -> Option<&dyn Plugin> {
        self.plugins
            .iter()
            .find(|p| p.name() == name)
            .map(|p| p.as_ref())
    }
}

/// Build a registry with all built-in Rust core format plugins.
/// Always-on plugins (timestamp, severity) are not in the registry —
/// the `Parser` runs them unconditionally after the format plugin.
pub fn build_default_registry() -> Registry {
    let mut r = Registry::new();
    r.register(Box::new(rust::jsonl::Jsonl));
    r.register(Box::new(rust::logfmt::Logfmt));
    r.register(Box::new(rust::syslog::Syslog));
    r.register(Box::new(rust::clf::Clf));
    r
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper for plugins: insert a field if the value is not null.
pub fn set_field(line: &mut ParsedLine, key: &str, value: FieldValue) {
    if matches!(value, FieldValue::Null) {
        return;
    }
    line.fields.insert(key.to_string(), value);
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Always;
    impl Plugin for Always {
        fn name(&self) -> &str {
            "always"
        }
        fn detect(&self, _s: &[&str]) -> f32 {
            1.0
        }
        fn parse(&self, _l: &mut ParsedLine) {}
    }

    #[test]
    fn registry_picks_highest_score() {
        let mut r = Registry::new();
        r.register(Box::new(Always));
        let p = r.detect_best(&["hi"]).unwrap();
        assert_eq!(p.name(), "always");
    }
}
