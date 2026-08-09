//! Command parsing: text line → `Command` enum.

/// A parsed REPL command.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// `open <path>` — open a file, spawning head + tail readers.
    Open { path: String },
    /// `show [--plain]` — render the current viewport as text.
    Show { plain: bool },
    /// `goto <line>` — scroll so that line N is at the top.
    Goto { line: u64 },
    /// `page <n>` — go to page N (0-indexed).
    Page { n: u64 },
    /// `home` — jump to the first line.
    Home,
    /// `end` — jump to the last line (enables follow).
    End,
    /// `follow on|off` — toggle follow mode.
    Follow { on: bool },
    /// `severity <level> on|off` — toggle visibility of a severity level.
    Severity { level: char, on: bool },
    /// `search <pattern>` — set active search pattern (regex).
    Search { pattern: String },
    /// `filter <expr>` — set a filter expression.
    Filter { expr: String },
    /// `sql <query>` — run a SQL query against the in-memory DB (phase 4).
    Sql { query: String },
    /// `histogram <bucket_secs>` — show a time histogram of line counts.
    Histogram { bucket_secs: i64 },
    /// `stats` — print runtime statistics.
    Stats,
    /// `lines <from> <count>` — dump raw lines starting at line N.
    Lines { from: u64, count: u64 },
    /// `fields <line>` — show extracted fields for a specific line.
    Fields { line: u64 },
    /// `json <line>` — pretty-print the JSON value for a JSONL line.
    Json { line: u64 },
    /// `readfile full|quick|x%` — block until the head reader reaches a milestone.
    ///   full  = block until the entire file is read
    ///   quick = return when the beginning and end are known (first lines + tail at EOF)
    ///   x%    = return when x% of the file (by bytes) has been read
    ReadFile { mode: ReadFileMode },
    /// `help` — list available commands.
    Help,
    /// `quit` — exit.
    Quit,
    /// Empty line (no-op).
    Empty,
    /// Unrecognized command.
    Unknown { raw: String },
}

/// Mode for the `readfile` command.
#[derive(Debug, Clone, PartialEq)]
pub enum ReadFileMode {
    /// Block until the head reader has read the entire file.
    Full,
    /// Return as soon as the first lines are available and the tail is at EOF.
    /// Since `open_dual` seeks the tail to EOF immediately, this effectively
    /// means "we have at least one screenful of lines from the head."
    Quick,
    /// Return when at least `percent` of the file (by bytes) has been read.
    Percent(f64),
}

/// Parse a single line of text into a `Command`.
pub fn parse(line: &str) -> Command {
    let line = line.trim();
    if line.is_empty() {
        return Command::Empty;
    }

    // Split into command word and the rest.
    let (cmd, rest) = match line.split_once(char::is_whitespace) {
        Some((c, r)) => (c, r.trim()),
        None => (line, ""),
    };

    match cmd {
        "open" | "o" => {
            if rest.is_empty() {
                return Command::Unknown {
                    raw: "open: missing path".into(),
                };
            }
            Command::Open {
                path: rest.to_string(),
            }
        }
        "show" | "s" => {
            let plain = rest == "--plain" || rest == "-p";
            Command::Show { plain }
        }
        "goto" | "g" => match rest.parse::<u64>() {
            Ok(n) => Command::Goto { line: n },
            Err(_) => Command::Unknown {
                raw: format!("goto: invalid line number '{rest}'"),
            },
        },
        "page" | "p" => match rest.parse::<u64>() {
            Ok(n) => Command::Page { n },
            Err(_) => Command::Unknown {
                raw: format!("page: invalid page number '{rest}'"),
            },
        },
        "home" | "H" => Command::Home,
        "end" | "E" => Command::End,
        "follow" => {
            let on = rest == "on" || rest == "true" || rest == "1";
            Command::Follow { on }
        }
        "severity" | "sev" => parse_severity(rest),
        "search" => Command::Search {
            pattern: rest.to_string(),
        },
        "filter" => Command::Filter {
            expr: rest.to_string(),
        },
        "sql" => Command::Sql {
            query: rest.to_string(),
        },
        "histogram" | "hist" => match rest.trim().parse::<i64>() {
            Ok(secs) => Command::Histogram { bucket_secs: secs },
            Err(_) => Command::Unknown {
                raw: format!("histogram: invalid bucket size '{rest}'"),
            },
        },
        "stats" => Command::Stats,
        "lines" | "l" => parse_lines(rest),
        "fields" | "f" => match rest.parse::<u64>() {
            Ok(n) => Command::Fields { line: n },
            Err(_) => Command::Unknown {
                raw: format!("fields: invalid line number '{rest}'"),
            },
        },
        "json" => match rest.parse::<u64>() {
            Ok(n) => Command::Json { line: n },
            Err(_) => Command::Unknown {
                raw: format!("json: invalid line number '{rest}'"),
            },
        },
        "wait" | "w" => {
            Command::Unknown {
                raw: "wait: removed — use 'readfile' instead".into()
            }
        }
        "readfile" | "rf" => parse_readfile(rest),
        "help" | "h" | "?" => Command::Help,
        "quit" | "q" | "exit" => Command::Quit,
        _ => Command::Unknown {
            raw: line.to_string(),
        },
    }
}

fn parse_severity(rest: &str) -> Command {
    // Format: <level> on|off  (e.g. "E off")
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() != 2 {
        return Command::Unknown {
            raw: format!("severity: expected '<level> on|off', got '{rest}'"),
        };
    }
    let level = parts[0].chars().next().unwrap_or('?');
    let on = parts[1] == "on" || parts[1] == "true" || parts[1] == "1";
    Command::Severity { level, on }
}

fn parse_lines(rest: &str) -> Command {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() != 2 {
        return Command::Unknown {
            raw: format!("lines: expected '<from> <count>', got '{rest}'"),
        };
    }
    match (parts[0].parse::<u64>(), parts[1].parse::<u64>()) {
        (Ok(from), Ok(count)) => Command::Lines { from, count },
        _ => Command::Unknown {
            raw: format!("lines: invalid arguments '{rest}'"),
        },
    }
}

fn parse_readfile(rest: &str) -> Command {
    let arg = rest.trim();
    if arg.is_empty() {
        return Command::Unknown {
            raw: "readfile: expected 'full', 'quick', or 'x%'".into(),
        };
    }
    match arg {
        "full" => Command::ReadFile {
            mode: ReadFileMode::Full,
        },
        "quick" => Command::ReadFile {
            mode: ReadFileMode::Quick,
        },
        _ => {
            // Try to parse as percentage (e.g. "50%", "50", "0.5")
            let s = arg.trim_end_matches('%');
            match s.parse::<f64>() {
                Ok(n) if n > 1.0 && n <= 100.0 => Command::ReadFile {
                    mode: ReadFileMode::Percent(n / 100.0),
                },
                Ok(n) if n > 0.0 && n <= 1.0 => Command::ReadFile {
                    mode: ReadFileMode::Percent(n),
                },
                _ => Command::Unknown {
                    raw: format!("readfile: invalid mode '{arg}' (expected full, quick, or x%)"),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_open() {
        assert_eq!(
            parse("open /tmp/log.jsonl"),
            Command::Open {
                path: "/tmp/log.jsonl".into()
            }
        );
    }

    #[test]
    fn parse_show_plain() {
        assert_eq!(parse("show --plain"), Command::Show { plain: true });
        assert_eq!(parse("show"), Command::Show { plain: false });
    }

    #[test]
    fn parse_goto() {
        assert_eq!(parse("goto 500"), Command::Goto { line: 500 });
        assert_eq!(parse("goto abc"), Command::Unknown { raw: "goto: invalid line number 'abc'".into() });
    }

    #[test]
    fn parse_severity() {
        assert_eq!(
            parse("severity E off"),
            Command::Severity { level: 'E', on: false }
        );
    }

    #[test]
    fn parse_lines() {
        assert_eq!(
            parse("lines 10 5"),
            Command::Lines { from: 10, count: 5 }
        );
    }

    #[test]
    fn parse_readfile_modes() {
        assert_eq!(
            parse("readfile full"),
            Command::ReadFile { mode: ReadFileMode::Full }
        );
        assert_eq!(
            parse("readfile quick"),
            Command::ReadFile { mode: ReadFileMode::Quick }
        );
        assert_eq!(
            parse("readfile 50%"),
            Command::ReadFile { mode: ReadFileMode::Percent(0.5) }
        );
        assert_eq!(
            parse("readfile 0.25"),
            Command::ReadFile { mode: ReadFileMode::Percent(0.25) }
        );
        assert!(matches!(parse("readfile bogus"), Command::Unknown { .. }));
    }

    #[test]
    fn parse_empty_and_unknown() {
        assert_eq!(parse(""), Command::Empty);
        assert_eq!(parse("   "), Command::Empty);
        assert!(matches!(parse("frobnicate"), Command::Unknown { .. }));
    }

    #[test]
    fn parse_quit_aliases() {
        assert_eq!(parse("quit"), Command::Quit);
        assert_eq!(parse("q"), Command::Quit);
        assert_eq!(parse("exit"), Command::Quit);
    }
}
