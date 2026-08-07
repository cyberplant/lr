//! Mutable application state shared by the UI and background tasks.

use std::path::PathBuf;

use crate::config::Config;
use crate::pipeline::parser::ParsedLine;
use crate::plugin::Severity;
use crate::theme::Theme;
use super::events::AppAction;

/// Runtime statistics shown in the status bar.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub total_lines: usize,
    pub lines_per_sec: f64,
}

/// The single source of truth for what the UI renders. Accessed only from
/// the main UI thread; background tasks communicate via channels that the
/// event loop drains.
pub struct AppState {
    pub config: Config,
    pub theme: Theme,
    pub files: Vec<PathBuf>,
    pub quit_requested: bool,

    /// Parsed lines ready for display.
    pub lines: Vec<ParsedLine>,
    /// Index of the first visible line (virtual scroll offset).
    pub scroll: usize,
    /// When true, auto-scroll to bottom as new lines arrive.
    pub follow: bool,
    /// Line wrapping toggle (not yet implemented in renderer).
    pub wrap: bool,
    /// Visible severity levels (all true by default).
    pub severity_visible: SeverityVisibility,

    pub stats: Stats,
    pub message: String,

    /// Last known terminal height (updated by the renderer each frame).
    pub terminal_height: u16,
}

#[derive(Debug, Clone, Default)]
pub struct SeverityVisibility {
    pub error: bool,
    pub warn: bool,
    pub info: bool,
    pub debug: bool,
    pub trace: bool,
}

impl SeverityVisibility {
    pub fn all_on() -> Self {
        Self {
            error: true,
            warn: true,
            info: true,
            debug: true,
            trace: true,
        }
    }

    pub fn is_visible(&self, sev: Severity) -> bool {
        match sev {
            Severity::Error => self.error,
            Severity::Warn => self.warn,
            Severity::Info => self.info,
            Severity::Debug => self.debug,
            Severity::Trace => self.trace,
        }
    }
}

impl AppState {
    pub fn new(config: Config, theme: Theme, files: Vec<PathBuf>, follow: bool) -> Self {
        let message = if files.is_empty() {
            "no files given — pass paths or --stdin".to_string()
        } else {
            format!("opened {} file(s)", files.len())
        };
        Self {
            config,
            theme,
            files,
            quit_requested: false,
            lines: Vec::new(),
            scroll: 0,
            follow, // head mode by default, follow only with -f
            wrap: false,
            severity_visible: SeverityVisibility::all_on(),
            stats: Stats::default(),
            message,
            terminal_height: 24,
        }
    }

    /// Push a new parsed line and update stats.
    pub fn push_line(&mut self, line: ParsedLine) {
        self.lines.push(line);
        self.stats.total_lines = self.lines.len();
    }

    /// Number of visible rows available for log lines (excluding status and
    /// command bars).
    pub fn visible_height(&self) -> usize {
        // terminal_height - 2 (status bar + command bar), minimum 1.
        (self.terminal_height as usize).saturating_sub(2).max(1)
    }

    /// Maximum scroll offset that keeps the last line visible.
    pub fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.visible_height())
    }

    /// Scroll to the bottom (last page).
    pub fn scroll_to_bottom(&mut self) {
        self.scroll = self.max_scroll();
    }

    /// Clamp scroll to valid range.
    fn clamp_scroll(&mut self) {
        let max = self.max_scroll();
        if self.scroll > max {
            self.scroll = max;
        }
    }

    pub fn apply(&mut self, action: AppAction) {
        match action {
            AppAction::Quit => self.quit_requested = true,
            AppAction::Noop => {}
            AppAction::ScrollDown => {
                self.follow = false;
                self.scroll = self.scroll.saturating_add(1);
                self.clamp_scroll();
            }
            AppAction::ScrollUp => {
                self.follow = false;
                self.scroll = self.scroll.saturating_sub(1);
            }
            AppAction::PageDown => {
                self.follow = false;
                self.scroll = self.scroll.saturating_add(self.visible_height());
                self.clamp_scroll();
            }
            AppAction::PageUp => {
                self.follow = false;
                self.scroll = self.scroll.saturating_sub(self.visible_height());
            }
            AppAction::Home => {
                self.follow = false;
                self.scroll = 0;
            }
            AppAction::End => {
                self.follow = true;
                self.scroll_to_bottom();
            }
            AppAction::ToggleFollow => {
                self.follow = !self.follow;
                if self.follow {
                    self.scroll_to_bottom();
                }
            }
            AppAction::PauseFollow => {
                self.follow = false;
            }
            AppAction::ToggleWrap => {
                self.wrap = !self.wrap;
            }
            AppAction::ToggleSeverityError => self.severity_visible.error = !self.severity_visible.error,
            AppAction::ToggleSeverityWarn => self.severity_visible.warn = !self.severity_visible.warn,
            AppAction::ToggleSeverityInfo => self.severity_visible.info = !self.severity_visible.info,
            AppAction::ToggleSeverityDebug => self.severity_visible.debug = !self.severity_visible.debug,
            AppAction::ToggleSeverityTrace => self.severity_visible.trace = !self.severity_visible.trace,
            AppAction::ToggleHelp => {
                self.message = "help overlay not implemented yet".to_string();
            }
            other => {
                self.message = format!("TODO: handle {other:?}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_clamps_to_max() {
        let mut s = AppState::new(Config::default(), Theme::default_theme(), vec![], false);
        s.terminal_height = 10;
        for i in 0..100 {
            s.push_line(ParsedLine::stub(&format!("line {i}")));
        }
        s.scroll = 200;
        s.clamp_scroll();
        assert_eq!(s.scroll, s.max_scroll());
    }

    #[test]
    fn home_disables_follow() {
        let mut s = AppState::new(Config::default(), Theme::default_theme(), vec![], false);
        s.follow = true;
        s.apply(AppAction::Home);
        assert!(!s.follow);
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn end_enables_follow() {
        let mut s = AppState::new(Config::default(), Theme::default_theme(), vec![], false);
        s.follow = false;
        for i in 0..50 {
            s.push_line(ParsedLine::stub(&format!("line {i}")));
        }
        s.apply(AppAction::End);
        assert!(s.follow);
        assert_eq!(s.scroll, s.max_scroll());
    }

    #[test]
    fn scroll_up_disables_follow() {
        let mut s = AppState::new(Config::default(), Theme::default_theme(), vec![], false);
        s.follow = true;
        s.apply(AppAction::ScrollUp);
        assert!(!s.follow);
    }
}
