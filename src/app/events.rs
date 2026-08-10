//! Maps crossterm key events to high-level `AppAction`s.
//!
//! Keybindings are hardcoded here for v1; phase 6 will make them configurable
//! via `config.toml` and consult that table first.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

/// A high-level action the app can perform. UI and state react to these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppAction {
    Noop,
    Quit,
    ToggleHelp,
    ScrollDown,
    ScrollUp,
    PageDown,
    PageUp,
    Home,
    End,
    ToggleFollow,
    PauseFollow,
    Search,
    NextMatch,
    PrevMatch,
    ClearSearch,
    CommandBar,
    ConfirmInput,
    CancelInput,
    ToggleSidebar,
    ToggleSeverityError,
    ToggleSeverityWarn,
    ToggleSeverityInfo,
    ToggleSeverityDebug,
    ToggleSeverityTrace,
    ToggleWrap,
    ToggleLineNumbers,
    ExpandEntry,
    Refresh,
}

pub fn map_event(key: KeyEvent) -> AppAction {
    // Ctrl+C always quits (escape hatch).
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return AppAction::Quit;
    }

    // Ctrl+L refreshes the UI (clear + redraw).
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('l') {
        return AppAction::Refresh;
    }

    match key.code {
        KeyCode::Char('q') => AppAction::Quit,
        KeyCode::Char('?') => AppAction::ToggleHelp,
        KeyCode::Char('j') | KeyCode::Down => AppAction::ScrollDown,
        KeyCode::Char('k') | KeyCode::Up => AppAction::ScrollUp,
        KeyCode::PageDown => AppAction::PageDown,
        KeyCode::PageUp => AppAction::PageUp,
        KeyCode::Home => AppAction::Home,
        KeyCode::End => AppAction::End,
        KeyCode::Char('f') => AppAction::ToggleFollow,
        KeyCode::Char('p') => AppAction::PauseFollow,
        KeyCode::Char('/') => AppAction::Search,
        KeyCode::Char('n') => AppAction::NextMatch,
        KeyCode::Char('N') => AppAction::PrevMatch,
        KeyCode::Esc => AppAction::ClearSearch,
        KeyCode::Char(':') => AppAction::CommandBar,
        KeyCode::Tab => AppAction::ToggleSidebar,
        KeyCode::Char('1') => AppAction::ToggleSeverityError,
        KeyCode::Char('2') => AppAction::ToggleSeverityWarn,
        KeyCode::Char('3') => AppAction::ToggleSeverityInfo,
        KeyCode::Char('4') => AppAction::ToggleSeverityDebug,
        KeyCode::Char('5') => AppAction::ToggleSeverityTrace,
        KeyCode::Char('w') => AppAction::ToggleWrap,
        KeyCode::Char('l') => AppAction::ToggleLineNumbers,
        KeyCode::Enter => AppAction::ExpandEntry,
        _ => AppAction::Noop,
    }
}

/// Map a key event when in search/command input mode.
/// Characters append to the buffer, Enter confirms, Esc cancels,
/// Backspace deletes.
pub fn map_input_event(key: KeyEvent) -> InputAction {
    match key.code {
        KeyCode::Enter => InputAction::Confirm,
        KeyCode::Esc => InputAction::Cancel,
        KeyCode::Backspace => InputAction::Backspace,
        KeyCode::Char(c) => InputAction::Char(c),
        _ => InputAction::Ignore,
    }
}

/// Action for input mode key handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    Char(char),
    Backspace,
    Confirm,
    Cancel,
    Ignore,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn ctrl_c_quits() {
        assert_eq!(
            map_event(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            AppAction::Quit
        );
    }

    #[test]
    fn q_quits() {
        assert_eq!(
            map_event(key(KeyCode::Char('q'), KeyModifiers::NONE)),
            AppAction::Quit
        );
    }

    #[test]
    fn home_end_map() {
        assert_eq!(
            map_event(key(KeyCode::Home, KeyModifiers::NONE)),
            AppAction::Home
        );
        assert_eq!(
            map_event(key(KeyCode::End, KeyModifiers::NONE)),
            AppAction::End
        );
    }

    #[test]
    fn ctrl_l_refreshes() {
        assert_eq!(
            map_event(key(KeyCode::Char('l'), KeyModifiers::CONTROL)),
            AppAction::Refresh
        );
    }
}
