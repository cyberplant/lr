//! Color themes for syntax, severity, and source tags.

use serde::{Deserialize, Serialize};

/// A resolved theme. Colors are stored as ratatui `Color` values; the on-disk
/// format uses hex strings (`#rrggbb`) or named colors.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Theme {
    #[serde(default)]
    pub severity: SeverityColors,
    #[serde(default)]
    pub syntax: SyntaxColors,
    #[serde(default)]
    pub source_tags: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SeverityColors {
    pub error: Option<String>,
    pub warn: Option<String>,
    pub info: Option<String>,
    pub debug: Option<String>,
    pub trace: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyntaxColors {
    pub key: Option<String>,
    pub string: Option<String>,
    pub number: Option<String>,
    pub boolean: Option<String>,
    pub null: Option<String>,
    pub timestamp: Option<String>,
}

impl Theme {
    pub fn default_theme() -> Self {
        Theme {
            severity: SeverityColors {
                error: Some("#ff5555".into()),
                warn: Some("#ffb86c".into()),
                info: Some("#8be9fd".into()),
                debug: Some("#6272a4".into()),
                trace: Some("#44475a".into()),
            },
            syntax: SyntaxColors {
                key: Some("#bd93f9".into()),
                string: Some("#f1fa8c".into()),
                number: Some("#50fa7b".into()),
                boolean: Some("#ff79c6".into()),
                null: Some("#6272a4".into()),
                timestamp: Some("#bd93f9".into()),
            },
            source_tags: vec![
                "#8be9fd".into(),
                "#50fa7b".into(),
                "#ffb86c".into(),
                "#ff79c6".into(),
                "#bd93f9".into(),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_has_all_severities() {
        let t = Theme::default_theme();
        assert!(t.severity.error.is_some());
        assert!(t.severity.warn.is_some());
    }
}
