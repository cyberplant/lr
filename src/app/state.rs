//! Mutable application state shared by the UI and background tasks.

use std::path::PathBuf;

use crate::config::Config;
use crate::filter::Filter;
use crate::pipeline::index::ReadProgress;
use crate::pipeline::parser::ParsedLine;
use crate::plugin::Severity;
use crate::search::Search;
use crate::theme::Theme;
use super::events::AppAction;

/// Runtime statistics shown in the status bar.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub total_lines: usize,
    pub lines_per_sec: f64,
}

/// Input mode for the command/search bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Search,
    Command,
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
    /// Show line numbers on the left (default off, toggle with 'l').
    pub show_line_numbers: bool,
    /// Visible severity levels (all true by default).
    pub severity_visible: SeverityVisibility,

    pub stats: Stats,
    pub message: String,

    /// Read progress tracker (shared with head/tail readers).
    pub progress: ReadProgress,

    /// Last known terminal height (updated by the renderer each frame).
    pub terminal_height: u16,

    // ── Search ──
    /// Current input mode (normal, search prompt, command prompt).
    pub input_mode: InputMode,
    /// Current text being typed in the input bar.
    pub input_buffer: String,
    /// Compiled search (if active).
    pub search: Option<Search>,
    /// Indices into `lines` that match the current search.
    pub search_matches: Vec<usize>,
    /// Current position in `search_matches` (for n/N navigation).
    pub search_cursor: usize,

    // ── Filter ──
    /// Active filter expression (if any). Lines not matching are hidden.
    pub filter: Option<Filter>,
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
            show_line_numbers: false,
            severity_visible: SeverityVisibility::all_on(),
            stats: Stats::default(),
            message,
            progress: ReadProgress::new(0),
            terminal_height: 24,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            search: None,
            search_matches: Vec::new(),
            search_cursor: 0,
            filter: None,
        }
    }

    /// Set the read progress tracker.
    pub fn set_progress(&mut self, progress: ReadProgress) {
        self.progress = progress;
    }

    // ── Search ──

    /// Compile a search pattern and find all matches in current lines.
    pub fn start_search(&mut self, pattern: &str) {
        match Search::new(pattern, false, false) {
            Ok(search) => {
                self.search = Some(search);
                self.recompute_search_matches();
                self.search_cursor = 0;
                // Jump to first match at or after current scroll.
                self.jump_to_nearest_match();
                self.message = if self.search_matches.is_empty() {
                    format!("search: no matches for '{pattern}'")
                } else {
                    format!("search: {} matches for '{pattern}'", self.search_matches.len())
                };
            }
            Err(e) => {
                self.message = format!("search error: {e}");
            }
        }
    }

    /// Clear the current search.
    pub fn clear_search(&mut self) {
        self.search = None;
        self.search_matches.clear();
        self.search_cursor = 0;
        self.message.clear();
    }

    /// Recompute search match indices. Called after new lines arrive.
    pub fn recompute_search_matches(&mut self) {
        if let Some(ref search) = self.search {
            self.search_matches = self
                .lines
                .iter()
                .enumerate()
                .filter(|(_, l)| search.is_match(&l.raw))
                .map(|(i, _)| i)
                .collect();
        }
    }

    /// Jump to the nearest match at or after the current scroll position.
    fn jump_to_nearest_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        // Find the first match index >= current scroll.
        let pos = self
            .search_matches
            .partition_point(|&idx| idx < self.scroll);
        if pos >= self.search_matches.len() {
            // Wrap around to the first match.
            self.search_cursor = 0;
        } else {
            self.search_cursor = pos;
        }
        let target = self.search_matches[self.search_cursor];
        self.scroll = target.min(self.max_scroll());
        self.follow = false;
    }

    /// Jump to the next search match.
    pub fn next_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        self.search_cursor = (self.search_cursor + 1) % self.search_matches.len();
        let target = self.search_matches[self.search_cursor];
        self.scroll = target.min(self.max_scroll());
        self.follow = false;
    }

    /// Jump to the previous search match.
    pub fn prev_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        if self.search_cursor == 0 {
            self.search_cursor = self.search_matches.len() - 1;
        } else {
            self.search_cursor -= 1;
        }
        let target = self.search_matches[self.search_cursor];
        self.scroll = target.min(self.max_scroll());
        self.follow = false;
    }

    // ── Filter ──

    /// Set a filter expression. Lines not matching will be hidden.
    pub fn set_filter(&mut self, filter: Filter) {
        self.filter = Some(filter);
        self.message = "filter applied".to_string();
    }

    /// Clear the current filter.
    pub fn clear_filter(&mut self) {
        self.filter = None;
        self.message = "filter cleared".to_string();
    }

    /// Check if a line passes the current filter (or if no filter is active).
    pub fn passes_filter(&self, line: &ParsedLine) -> bool {
        self.filter.as_ref().is_none_or(|f| f.matches(line))
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
        // In search/command input mode, most actions are handled by the
        // input handler. Only Quit and the input-specific actions apply.
        if self.input_mode != InputMode::Normal {
            match action {
                AppAction::Quit => self.quit_requested = true,
                AppAction::ConfirmInput => {
                    let buf = std::mem::take(&mut self.input_buffer);
                    match self.input_mode {
                        InputMode::Search => self.start_search(&buf),
                        InputMode::Command => self.execute_command(&buf),
                        InputMode::Normal => {}
                    }
                    self.input_mode = InputMode::Normal;
                }
                AppAction::CancelInput => {
                    self.input_mode = InputMode::Normal;
                    self.input_buffer.clear();
                }
                _ => {}
            }
            return;
        }

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
            AppAction::ToggleLineNumbers => {
                self.show_line_numbers = !self.show_line_numbers;
            }
            AppAction::ToggleSeverityError => self.severity_visible.error = !self.severity_visible.error,
            AppAction::ToggleSeverityWarn => self.severity_visible.warn = !self.severity_visible.warn,
            AppAction::ToggleSeverityInfo => self.severity_visible.info = !self.severity_visible.info,
            AppAction::ToggleSeverityDebug => self.severity_visible.debug = !self.severity_visible.debug,
            AppAction::ToggleSeverityTrace => self.severity_visible.trace = !self.severity_visible.trace,
            AppAction::Search => {
                self.input_mode = InputMode::Search;
                self.input_buffer.clear();
            }
            AppAction::CommandBar => {
                self.input_mode = InputMode::Command;
                self.input_buffer.clear();
            }
            AppAction::NextMatch => self.next_match(),
            AppAction::PrevMatch => self.prev_match(),
            AppAction::ClearSearch => self.clear_search(),
            AppAction::ToggleHelp => {
                self.message = "help overlay not implemented yet".to_string();
            }
            other => {
                self.message = format!("TODO: handle {other:?}");
            }
        }
    }

    /// Execute a command from the command bar.
    fn execute_command(&mut self, cmd: &str) {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            return;
        }
        if cmd == "clear" || cmd == "filter clear" {
            self.clear_filter();
            self.clear_search();
            return;
        }
        if let Some(rest) = cmd.strip_prefix("filter ") {
            if let Some(f) = parse_filter(rest) {
                self.set_filter(f);
            } else {
                self.message = format!("invalid filter: {rest}");
            }
            return;
        }
        if let Some(rest) = cmd.strip_prefix("search ") {
            self.start_search(rest);
            return;
        }
        self.message = format!("unknown command: {cmd}");
    }
}

/// Parse a filter expression from a string.
/// Supported syntax:
///   severity=ERROR          — match severity
///   field=key=value          — match field equality
///   /regex/                  — match raw text against regex
///   a and b                  — both must match
///   a or b                   — either must match
///   not a                    — negation
fn parse_filter(s: &str) -> Option<Filter> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    // Split on " or " (lowest precedence).
    let or_parts: Vec<&str> = split_top_level(s, " or ");
    if or_parts.len() > 1 {
        let filters: Vec<Filter> = or_parts.iter().filter_map(|p| parse_filter(p)).collect();
        if filters.is_empty() {
            return None;
        }
        return Some(Filter::Any(filters));
    }

    // Split on " and ".
    let and_parts: Vec<&str> = split_top_level(s, " and ");
    if and_parts.len() > 1 {
        let filters: Vec<Filter> = and_parts.iter().filter_map(|p| parse_filter(p)).collect();
        if filters.is_empty() {
            return None;
        }
        return Some(Filter::All(filters));
    }

    // Not.
    if let Some(rest) = s.strip_prefix("not ") {
        return parse_filter(rest).map(|f| Filter::Not(Box::new(f)));
    }

    // severity=X
    if let Some(rest) = s.strip_prefix("severity=") {
        let sev = match rest.to_ascii_lowercase().as_str() {
            "error" | "err" => Severity::Error,
            "warn" | "warning" => Severity::Warn,
            "info" => Severity::Info,
            "debug" => Severity::Debug,
            "trace" => Severity::Trace,
            _ => return None,
        };
        return Some(Filter::Severity(sev));
    }

    // field=key=value
    if let Some(rest) = s.strip_prefix("field=") {
        if let Some(eq_pos) = rest.find('=') {
            let key = rest[..eq_pos].to_string();
            let value = rest[eq_pos + 1..].to_string();
            return Some(Filter::FieldEq { key, value });
        }
        return None;
    }

    // /regex/
    if s.starts_with('/') && s.ends_with('/') && s.len() > 2 {
        let pattern = &s[1..s.len() - 1];
        return regex::Regex::new(pattern).ok().map(Filter::Regex);
    }

    // Bare regex (no slashes).
    regex::Regex::new(s).ok().map(Filter::Regex)
}

/// Split a string on a separator, but only at the top level
/// (not inside slashes or quotes). Simple version for now.
fn split_top_level<'a>(s: &'a str, sep: &str) -> Vec<&'a str> {
    s.split(sep).collect()
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

    #[test]
    fn search_finds_matches() {
        let mut s = AppState::new(Config::default(), Theme::default_theme(), vec![], false);
        s.push_line(ParsedLine::stub("error: something broke"));
        s.push_line(ParsedLine::stub("info: all good"));
        s.push_line(ParsedLine::stub("error: again"));
        s.start_search("error");
        assert_eq!(s.search_matches.len(), 2);
        assert_eq!(s.search_matches, vec![0, 2]);
    }

    #[test]
    fn search_next_prev_navigation() {
        let mut s = AppState::new(Config::default(), Theme::default_theme(), vec![], false);
        for i in 0..10 {
            s.push_line(ParsedLine::stub(&format!("line {i} error")));
        }
        s.start_search("error");
        assert_eq!(s.search_matches.len(), 10);
        // First match should be at index 0.
        assert_eq!(s.search_cursor, 0);
        s.next_match();
        assert_eq!(s.search_cursor, 1);
        s.next_match();
        assert_eq!(s.search_cursor, 2);
        s.prev_match();
        assert_eq!(s.search_cursor, 1);
        // Wrap around: prev from 0 goes to last.
        s.search_cursor = 0;
        s.prev_match();
        assert_eq!(s.search_cursor, 9);
        // Wrap around: next from last goes to 0.
        s.search_cursor = 9;
        s.next_match();
        assert_eq!(s.search_cursor, 0);
    }

    #[test]
    fn search_clear() {
        let mut s = AppState::new(Config::default(), Theme::default_theme(), vec![], false);
        s.push_line(ParsedLine::stub("error: broke"));
        s.start_search("error");
        assert!(s.search.is_some());
        assert!(!s.search_matches.is_empty());
        s.clear_search();
        assert!(s.search.is_none());
        assert!(s.search_matches.is_empty());
    }

    #[test]
    fn filter_hides_non_matching() {
        let mut s = AppState::new(Config::default(), Theme::default_theme(), vec![], false);
        let mut line1 = ParsedLine::stub("error: broke");
        line1.severity = Some(Severity::Error);
        let mut line2 = ParsedLine::stub("info: ok");
        line2.severity = Some(Severity::Info);
        s.push_line(line1);
        s.push_line(line2);
        s.set_filter(Filter::Severity(Severity::Error));
        assert!(s.passes_filter(&s.lines[0]));
        assert!(!s.passes_filter(&s.lines[1]));
        s.clear_filter();
        assert!(s.passes_filter(&s.lines[1]));
    }

    #[test]
    fn parse_filter_severity() {
        let f = parse_filter("severity=error").unwrap();
        let mut line = ParsedLine::stub("x");
        line.severity = Some(Severity::Error);
        assert!(f.matches(&line));
    }

    #[test]
    fn parse_filter_field_eq() {
        let f = parse_filter("field=user=alice").unwrap();
        let mut line = ParsedLine::stub("x");
        line.fields.insert("user".into(), crate::pipeline::parser::FieldValue::Str("alice".into()));
        assert!(f.matches(&line));
    }

    #[test]
    fn parse_filter_and() {
        let f = parse_filter("severity=error and broke").unwrap();
        let mut line = ParsedLine::stub("error: broke");
        line.severity = Some(Severity::Error);
        assert!(f.matches(&line));
    }

    #[test]
    fn parse_filter_regex() {
        let f = parse_filter("/err.*/").unwrap();
        assert!(f.matches(&ParsedLine::stub("an error occurred")));
        assert!(!f.matches(&ParsedLine::stub("all good")));
    }
}
