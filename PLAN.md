---
agent: devin-local
session: octagonal-degree
created: 2026-08-07T03:02:51Z
---
# LR — Log Reader (Rust)

A fast, plugin-driven terminal log reader in Rust that opens a file with two file descriptors (one streaming from the start, one tailing the end), parses lines through plugins, and presents an interactive TUI with search, filters, severity detection, JSONL pretty-printing, and an in-memory SQLite database for querying fields and rendering time-bucket histograms.

## Goals & Non-Goals

### Goals (v1)
- **Fast startup**: open the file with two FDs — a head reader streaming from byte 0, and a tail reader holding the end position and following appends. User can hit `HOME`/`END` immediately while parsing continues in the background.
- **Plugin architecture**: Rust core plugins (compiled in, fast) for built-in formats + Lua (via `mlua` with LuaJIT) for user-contributed parsers/filters/transforms.
- **TUI** built on `ratatui` + `crossterm`, async via `tokio`.
- **Built-in plugins**: `jsonl` (parse + syntax color + JSONPath queries + collapsible nested pretty-print), `database` (SQLite in-memory, field queries, time-bucket histograms), plus format auto-detection plugins for `logfmt`, `syslog`, `CLF/NCSA`, `regex-capture-to-fields`, `key=value`, `ANSI strip/passthrough`.
- **Search**: regex + literal + case-insensitive, match highlighting, next/prev match navigation.
- **Filters**: first-class, saved filter expressions, combinable with AND/OR.
- **Severity detection + filtering** (ERROR/WARN/INFO/DEBUG) with hotkeys.
- **Timestamp normalization** across formats (epoch, ISO8601, syslog, custom) — enables sorting and histogram bucketing.
- **Follow/tail mode** with pause-on-new-input and "back to live" hotkey.
- **Multiple files / merged view** with color-coded source tag per line.
- **Virtual scrolling**: line wrap on/off, line numbers, relative timestamps.
- **Pretty-print nested JSON** (collapsible, toolong-style).
- **Stats bar**: lines/sec, total bytes, time range covered, current filter match count.
- **Delta view**: only show lines new since last pause.
- **Session persistence**: remember last file, scroll position, filters, column widths.
- **Config file** `~/.config/lr/config.toml` + keybinding remapping.
- **Theming**: syntax colors, severity colors, user themes.

### Non-Goals (explicitly out of v1)
- Marks/bookmarks on lines.
- Remote sources (SSH, HTTP, Kafka, journald) — local files + stdin only for v1.
- Export (pipe out / save view to file).
- Diff mode (compare two logs side by side).
- Separate grep mode (search + filters cover this).

## Architecture

### Tech Stack
- **Language**: Rust (edition 2021).
- **Async runtime**: `tokio` (multi-threaded).
- **TUI**: `ratatui` + `crossterm`.
- **Plugins**: Rust core (trait objects) + `mlua` with LuaJIT feature for user plugins.
- **DB**: `rusqlite` (SQLite in-memory), behind a storage trait so DuckDB could be swapped later.
- **Config**: `serde` + `toml`.
- **Logging/diagnostics**: `tracing` + `tracing-subscriber` to a debug log file (never the terminal — it's the TUI).

### Core Modules (planned crate layout)
```
lr/
├── Cargo.toml
├── src/
│   ├── main.rs                 # CLI args (clap), bootstrap, spawn tasks, run TUI
│   ├── cli.rs                  # arg parsing, --config, --theme, file list, --stdin
│   ├── config.rs               # load ~/.config/lr/config.toml, keybindings, theme
│   ├── theme.rs                # color/theme definitions
│   ├── app/
│   │   ├── mod.rs
│   │   ├── state.rs            # App state: open files, current view, filters, mode
│   │   └── events.rs           # keyboard/mouse → AppAction mapping (configurable)
│   ├── io/
│   │   ├── mod.rs
│   │   ├── file.rs             # open file, two FDs (head + tail), seek to end
│   │   ├── tail.rs             # tail-follow loop (tokio task), notify/inotify/kqueue
│   │   ├── stdin.rs            # stdin streaming source
│   │   └── line_buffer.rs      # bounded ring of parsed lines, backpressure
│   ├── pipeline/
│   │   ├── mod.rs
│   │   ├── parser.rs           # drives plugins per line, produces ParsedLine
│   │   └── index.rs            # builds line-offset index for jump-to-byte
│   ├── plugin/
│   │   ├── mod.rs              # Plugin trait, registry, plugin manager
│   │   ├── rust/
│   │   │   ├── mod.rs
│   │   │   ├── detect.rs       # format auto-detection (sample first N lines)
│   │   │   ├── jsonl.rs        # JSONL parse + syntax color + JSONPath
│   │   │   ├── logfmt.rs       # key=value / logfmt
│   │   │   ├── syslog.rs       # syslog format
│   │   │   ├── clf.rs          # common log format / NCSA
│   │   │   ├── regex_capture.rs # named captures → queryable fields
│   │   │   ├── ansi.rs         # ANSI strip/passthrough
│   │   │   ├── severity.rs     # level detection (ERROR/WARN/INFO/DEBUG)
│   │   │   └── timestamp.rs    # timestamp normalization to epoch
│   │   └── lua/
│   │       ├── mod.rs          # mlua engine, sandbox, host API
│   │       └── api.rs          # functions exposed to Lua (register_parser, etc.)
│   ├── db/
│   │   ├── mod.rs              # Storage trait
│   │   └── sqlite.rs           # rusqlite impl, schema, insert, query, histogram
│   ├── search/
│   │   └── mod.rs              # regex/literal search, match index, next/prev
│   ├── filter/
│   │   └── mod.rs              # filter expression AST, AND/OR, saved filters
│   └── ui/
│       ├── mod.rs              # ratatui App loop, layout
│       ├── view.rs             # main log view (virtual scroll)
│       ├── statusbar.rs        # stats bar (lines/sec, bytes, time range, matches)
│       ├── commandbar.rs       # command/filter/search input line
│       ├── sidebar.rs          # histogram + field query panel (DB plugin)
│       ├── json_view.rs        # collapsible JSON pretty-print pane
│       └── help.rs             # help overlay
├── plugins/                    # example Lua plugins
│   └── example.lua
└── themes/
    └── default.toml
```

### Data Flow
1. `main` parses CLI, loads config/theme, opens each file via `io::file::open_dual` → returns `(head_reader, tail_reader)`.
2. Spawn per-source tokio tasks:
   - **head task**: reads from byte 0 forward, feeds raw bytes into `pipeline::parser`.
   - **tail task**: seeks to end, polls for appends (kqueue on macOS, inotify on Linux), feeds new bytes into `pipeline::parser`.
3. `pipeline::parser` runs each line through the active plugin chain (Rust core first for detection/normalization, then Lua user plugins), producing a `ParsedLine { raw, timestamp, severity, fields: HashMap<String, Value>, source_tag, byte_offset }`.
4. `ParsedLine` is pushed into `io::line_buffer` (bounded; backpressure pauses the reader) **and** inserted into the DB (async, batched).
5. UI reads from `line_buffer` via virtual scrolling; search/filter operate on the buffer and DB.
6. User actions (HOME/END/search/filter/toggle-severity/toggle-follow) mutate `app::state`; UI re-renders.

### Plugin Model
- **Rust core plugins**: implement `trait Plugin { fn detect(&self, sample: &[&str]) -> f32; fn parse(&self, line: &mut ParsedLine); }`. Registered at startup, auto-selected by detection confidence score. Always available, fast.
- **Lua plugins**: loaded from `~/.config/lr/plugins/*.lua` and `./.lr/plugins/*.lua`. Sandbox via `mlua` with a restricted API: `register_parser{name=, detect=, parse=}`, `extract_field()`, `set_severity()`, `set_timestamp()`. LuaJIT enabled for speed. Lua plugins run *after* Rust core plugins (can augment, not replace, core detection).
- **Plugin manager**: resolves conflicts by detection score; user can force a plugin via config or `--plugin`.

### DB Plugin (SQLite)
- Schema: `lines(id INTEGER PK, ts INTEGER, ts_ns INTEGER, severity TEXT, source TEXT, byte_offset INTEGER, raw TEXT)` + an `EAV`-style `fields(line_id, key, value)` for extracted fields, plus a JSON column for the full structured payload.
- Insert path: batched (every N lines or M ms) via a dedicated tokio task reading from a channel.
- Query panel (sidebar): user types SQL or uses a query builder; results render as a table. Common queries preset (count by severity, count by 1-min bucket, top fields).
- Histogram: rendered as a horizontal bar chart in the sidebar, bucket size auto-chosen from time range (1s/1m/1h/1d).

### TUI Layout
```
┌─────────────────────────────────────────────┬──────────────┐
│  Log view (virtual scroll)                  │  Sidebar     │
│  [src] [sev] [ts]  raw line / pretty json   │  histogram   │
│  ...                                        │  field query │
│                                             │  results     │
├─────────────────────────────────────────────┴──────────────┤
│  status: 12,453 lines | 4.2MB | 1m ago–now | 321 matches   │
├────────────────────────────────────────────────────────────┤
│  :filter severity=ERROR and $.user.id=42                   │
└────────────────────────────────────────────────────────────┘
```
- `HOME`/`END`: jump to start/end of buffer; `END` enables follow mode.
- `f`: toggle follow (tail). `p`: pause follow on new input. `Space`: page down.
- `/`: search (regex by default, `"` for literal). `n`/`N`: next/prev match.
- `:`: command bar (filters, queries, theme, plugin commands).
- `1`-`9`: toggle severity visibility. `j`/`k`: scroll. `Enter`: expand JSON / open detail.
- `?`: help overlay. `q`: quit.

## Implementation Steps (ordered)

### Phase 0 — Skeleton
1. `cargo init`, set up `Cargo.toml` with deps: `tokio`, `ratatui`, `crossterm`, `clap`, `serde`, `toml`, `rusqlite`, `mlua` (lua54 + luajit feature), `regex`, `tracing`, `anyhow`, `serde_json`.
2. `main.rs`: parse CLI args (file paths, `--stdin`, `--config`, `--theme`, `--plugin`), set up `tracing` to `~/.local/share/lr/debug.log`, hand off to `app::run`.
3. `app::run`: initialize `crossterm`, enter raw mode, create `ratatui::Terminal`, run event loop reading `crossterm::event` with `tokio::select!` over input + internal channels. Clean teardown on panic/exit (restore terminal).

### Phase 1 — Dual-FD file reader
4. `io::file::open_dual(path)`: open file twice (or dup FD), seek one to 0, seek one to end. Return both handles + file size.
5. `io::tail`: tokio task that uses `kqueue` (macOS) / `inotify` (Linux) via the `notify` crate to watch for appends; on event, reads new bytes and sends them on a channel. Fallback: poll file size every 250ms.
6. `io::line_buffer`: bounded `VecDeque<ParsedLine>` with capacity (configurable, default 100k lines). When full, apply backpressure by not draining the reader channel (tokio natural backpressure).
7. Head reader task: read forward in chunks, split on newlines, send raw lines to parser pipeline.

### Phase 2 — Plugin system + core plugins
8. Define `ParsedLine` struct and `Plugin` trait in `plugin/mod.rs`. Build registry with detection scoring.
9. `plugin::detect`: sample first 64 lines, run each plugin's `detect`, pick highest score (tie-break by config order).
10. Implement Rust core plugins: `timestamp` (normalize to epoch ns), `severity` (regex on common level words + syslog priorities), `jsonl` (parse, syntax-color via ratatui spans, extract fields, support `$.path` queries), `logfmt`, `syslog`, `clf`, `regex_capture` (user-supplied regex with named groups → fields), `ansi` (strip or passthrough).
11. `pipeline::parser`: orchestrates plugin chain per line; emits `ParsedLine` to buffer + DB channel.

### Phase 3 — Lua plugin host
12. `plugin::lua`: init `mlua::Lua` with LuaJIT, register sandboxed API (`register_parser`, `extract_field`, `set_severity`, `set_timestamp`, `log_debug`). Load `.lua` files from config dirs. Map Lua tables ↔ `ParsedLine` fields.
13. Write one example Lua plugin (`plugins/example.lua`) demonstrating a custom format parser, to validate the API ergonomics.

### Phase 4 — DB plugin
14. `db::sqlite`: open in-memory DB, create schema, prepared insert statements. Batched insert task reading from a channel.
15. Query interface: `query(sql) -> Vec<Row>`, `histogram(bucket_secs) -> Vec<(bucket, count)>`.
16. Wire DB inserts into the pipeline (after parsing).

### Phase 5 — UI
17. `ui::view`: virtual scrolling log view. Render visible window from `line_buffer` using byte-offset index for fast seek. Support wrap on/off, line numbers, relative timestamps, source tag, severity color.
18. `ui::statusbar`: lines/sec (EWMA), total bytes, time range, match count.
19. `ui::commandbar`: `:` prompt with history; parse filter expressions and SQL; `/` search prompt.
20. `ui::sidebar`: histogram bar chart (ratatui `BarChart` widget) + query results table. Toggle with `Tab`.
21. `ui::json_view`: collapsible JSON tree pane (open on `Enter` over a JSONL line).
22. `ui::help`: `?` overlay listing keybindings from config.

### Phase 6 — Search & filters
23. `search`: compile regex/literal, scan buffer (or DB for field queries), build match index, `n`/`N` navigation with viewport jump.
24. `filter`: AST for `severity=X and $.field=Y or /regex/`, evaluate per line, hide non-matches, save named filters in config.

### Phase 7 — Modes & quality of life
25. Follow/tail mode + pause-on-input + back-to-live (`END`/`f`).
26. Delta view (only lines since last pause).
27. Multiple files merged view with source tags.
28. Session persistence: save/restore last file, scroll pos, filters, column widths to `~/.local/share/lr/session.toml`.
29. Config file `~/.config/lr/config.toml`: keybindings, theme, default plugins, buffer size.
30. Theming: `themes/default.toml` + user themes; severity/syntax/source colors.

## Files to Create (initial)
- `Cargo.toml` — dependencies and features.
- `src/main.rs`, `src/cli.rs`, `src/config.rs`, `src/theme.rs`
- `src/app/{mod,state,events}.rs`
- `src/io/{mod,file,tail,stdin,line_buffer}.rs`
- `src/pipeline/{mod,parser,index}.rs`
- `src/plugin/{mod,rust/mod,detect,jsonl,logfmt,syslog,clf,regex_capture,ansi,severity,timestamp,lua/mod,api}.rs`
- `src/db/{mod,sqlite}.rs`
- `src/search/mod.rs`, `src/filter/mod.rs`
- `src/ui/{mod,view,statusbar,commandbar,sidebar,json_view,help}.rs`
- `plugins/example.lua`, `themes/default.toml`
- `AGENTS.md` — build/test commands and conventions for this repo.

## Verification
- [ ] `cargo build` compiles with no warnings (deny warnings in CI config).
- [ ] `cargo clippy -- -D warnings` clean.
- [ ] `cargo test` — unit tests for: line splitter, timestamp normalizer (all formats), severity detector, jsonl parser + JSONPath, logfmt parser, filter AST eval, DB insert+query+histogram, Lua plugin round-trip.
- [ ] Manual: open a 1GB JSONL log → `HOME` and `END` respond within 200ms while parsing continues; tail mode follows appends from `echo >> file`.
- [ ] Manual: severity filter hotkeys hide/show levels instantly.
- [ ] Manual: histogram sidebar renders correct buckets for a known file.
- [ ] Manual: a Lua plugin from `~/.config/lr/plugins/` loads and parses a custom format.
- [ ] Manual: search `/error` highlights matches and `n`/`N` navigates.
- [ ] Manual: quit with `q` restores terminal cleanly even mid-parse.

## Risks / Considerations
- **Two-FD correctness**: must handle truncation/rotation (file replaced under the tail FD). v1 plan: detect size shrink → re-open tail FD at 0; full rotation (new inode) is a known gap, document it.
- **Memory bounds**: `line_buffer` is bounded; DB grows unbounded in-memory. Add a configurable max-rows cap with oldest-eviction, and warn the user.
- **Backpressure**: if parsing/DB can't keep up with a fast tail, the head reader must not starve the tail. Use separate channels and prioritize tail.
- **macOS kqueue vs Linux inotify**: use the `notify` crate to abstract; verify both paths.
- **Lua sandboxing**: `mlua` sandbox must deny os/io/file access by default; provide a config flag `lua_unsafe = true` for trusted power users.
- **Plugin detection ambiguity**: first 64 lines may mislead (e.g., mixed format). Allow manual override via `--plugin` and per-file config.
- **Large JSONL lines**: pretty-print/collapsible view must handle multi-MB single lines without freezing the UI — cap rendered depth/length, show truncation marker.
- **rusqlite + tokio**: rusqlite is sync; run DB ops on a `tokio::task::spawn_blocking` or a dedicated DB thread with a channel. Do not hold the DB lock across await points.
- **DuckDB swap path**: keep DB behind a `Storage` trait so the analytical-scaling upgrade is feasible later without rewriting the pipeline.
