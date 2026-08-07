//! Mutable application state shared by the UI and background tasks.

use std::path::PathBuf;

use crate::config::Config;
use crate::theme::Theme;
use super::events::AppAction;

/// The single source of truth for what the UI renders and what background
/// tasks have produced so far. Accessed only from the main UI thread in v1;
/// background tasks communicate via channels that the event loop drains.
pub struct AppState {
    pub config: Config,
    pub theme: Theme,
    pub files: Vec<PathBuf>,
    pub quit_requested: bool,

    // TODO(phase 1): `line_buffer: LineBuffer`
    // TODO(phase 4): `db: Option<DbHandle>`
    // TODO(phase 6): `filters: Vec<Filter>`, `search: Option<Search>`
    pub message: String,
}

impl AppState {
    pub fn new(config: Config, theme: Theme, files: Vec<PathBuf>) -> Self {
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
            message,
        }
    }

    pub fn apply(&mut self, action: AppAction) {
        match action {
            AppAction::Quit => self.quit_requested = true,
            AppAction::Noop => {}
            AppAction::ToggleHelp => {
                self.message = "help overlay not implemented yet".to_string();
            }
            other => {
                self.message = format!("TODO: handle {other:?}");
            }
        }
    }
}
