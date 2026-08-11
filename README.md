# LR — Log Reader

A fast, plugin-driven terminal log reader written in Rust.

LR opens log files with two file descriptors — one streaming from the start,
one tailing the end — so you can hit `HOME`/`END` instantly while parsing
continues in the background. It parses lines through a plugin chain (format
detection, timestamp normalization, severity extraction), stores them in an
in-memory SQLite database for SQL queries and time histograms, and presents
an interactive TUI with cursor navigation, search, filtering, and a
sparkline rate graph.

## Features

- **Dual-FD reader**: head reader streams from byte 0, tail reader follows
  appends via OS-native file watchers (FSEvents/inotify/ReadDirectoryChangesW)
- **Plugin architecture**: Rust core plugins for syslog, CLF/NCSA, JSONL,
  logfmt, key=value, regex capture, ANSI stripping, timestamp normalization,
  and severity detection
- **In-memory SQLite**: query parsed fields with SQL, generate time-bucketed
  histograms
- **TUI** built on ratatui + crossterm, async via tokio
- **Cursor navigation**: highlighted cursor line, HOME/END/PgUp/PgDn/arrows
- **Search**: regex + literal + case-insensitive, match highlighting, n/N
  navigation
- **Filters**: severity toggles (1-5 hotkeys), filter expression AST
  (severity=, field=, regex, AND/OR/NOT)
- **Rate sparkline**: 5-character block graph showing lines/sec trend over
  the last 30 minutes
- **REPL/TCP mode**: scriptable command interface when stdout is not a TTY
- **Control char sanitization**: escape sequences in log content are rendered
  as `<XX>` hex escapes to prevent terminal corruption

## Installation

### From source

```sh
cargo install --path .
```

### From crates.io

```sh
cargo install lr
```

### Build from source

```sh
git clone https://github.com/cyberplant/lr.git
cd lr
cargo build --release
# Binary is at target/release/lr
```

## Usage

```sh
# Open a file in the TUI (head mode — starts at the beginning)
lr app.log

# Follow mode (like tail -f)
lr -f app.log

# Read from stdin
echo "hello" | lr --stdin

# Multiple files
lr app.log error.log

# REPL mode (when stdout is piped, or with --repl)
echo "show" | lr app.log --repl

# JSON output mode
lr app.log --repl --json

# TCP command server
lr app.log --listen 127.0.0.1:9999
```

## Keybindings

| Key | Action |
|-----|--------|
| `j` / `Down` | Move cursor down |
| `k` / `Up` | Move cursor up |
| `PgDn` | Move cursor down one page |
| `PgUp` | Move cursor up one page |
| `Home` | Jump to first line |
| `End` | Jump to last line (follow mode) |
| `f` | Toggle follow mode |
| `p` | Pause follow |
| `/` | Search (regex) |
| `n` / `N` | Next / previous match |
| `Esc` | Clear search |
| `:` | Command bar |
| `1`-`5` | Toggle severity filters (E/W/I/D/T) |
| `l` | Toggle line numbers |
| `w` | Toggle line wrapping |
| `Tab` | Toggle sidebar |
| `Ctrl+L` | Refresh UI |
| `Ctrl+C` / `q` | Quit |

## REPL Commands

When stdout is not a TTY (or `--repl` is given), LR enters REPL mode:

```
open <path>              Open a file
readfile full|quick|x%   Read file (full=entire, quick=first lines+tail, x%=percentage)
show                     Show current viewport
goto <line>              Go to line number
home / end               Jump to start/end
page <n>                 Go to page n
follow on|off            Toggle follow mode
stats                    Show statistics
lines <from> <count>     Dump raw lines
fields <line>            Show extracted fields for a line
json <line>              Pretty-print JSON for a line
search <pattern>         Set search pattern
filter <expr>            Set filter expression
sql <query>              Run SQL query against the in-memory DB
histogram <bucket_secs>  Show time histogram of line counts
help                     Show help
quit                     Exit
```

## Architecture

```
readers → [raw channel] → parser → [parsed channel] → UI
                                      ↓
                                 [DB channel] → SQLite writer
```

- **Head reader**: reads from byte 0 to `tail_start` (EOF - 64KB)
- **Tail reader**: seeks to `tail_start`, reads to EOF, then follows appends
  via notify (FSEvents/inotify/ReadDirectoryChangesW)
- **Parser**: runs plugin chain (detect → timestamp → severity → format-specific)
- **DB writer**: batch-inserts parsed lines into in-memory SQLite
- **UI**: drains parsed lines, renders at ~30fps, polls keyboard

## License

BSD 3-Clause. See [LICENSE](LICENSE).
