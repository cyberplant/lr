# LR — Log Reader

A fast, plugin-driven terminal log reader in Rust. See `PLAN.md` for the full
architecture and roadmap.

## Build & Test

```sh
cargo build              # build
cargo test               # run unit tests (no tty required)
cargo clippy --all-targets -- -D warnings   # lint (must be clean)
cargo run -- --print-config                 # print effective config (no tty)
cargo run -- <file>                         # open a file in the TUI
cargo run -- --stdin                        # read from stdin
```

## Debug log

`lr` writes diagnostics to `~/.local/share/lr/debug.log` (never the terminal,
which is the TUI). Verbosity is controlled by `--log-level` (error/warn/info/
debug/trace).

## Conventions

- **Edition 2024**, Rust 1.85+ required (developed on 1.90).
- **No emojis** in code or commit messages unless explicitly requested.
- **Comments**: do not add or remove comments unless asked. Existing comments
  must be preserved across edits.
- **Clippy** must be clean with `-D warnings`. The crate currently has
  `#![allow(dead_code)]` at the crate root because many modules are stubs for
  later phases; remove this once the pipeline is fully connected.
- **Dependencies**: pinned to versions compatible with stable Rust. Note:
  `rusqlite` is pinned to `0.32` because `0.40` requires nightly
  (`cfg_select!` is unstable). `mlua` uses the `luajit` feature only (not
  `lua54` + `luajit` together — mlua forbids multiple Lua variants).
- **Architecture**: see `PLAN.md`. Phases 0-7 are defined there. Each phase
  has explicit TODOs in the source marked `TODO(phase N)`.

## REPL / TCP mode

When stdout is not a TTY (piped), or `--repl` is given, lr enters REPL mode
instead of the TUI. Commands are read from stdin and responses printed to
stdout. Use `--json` for JSON output. Use `--listen <addr:port>` to start a
TCP command server (can be combined with `--repl`).

Key commands: `open`, `readfile full|quick|x%`, `show`, `goto`, `stats`,
`fields`, `json`, `severity`, `follow`, `help`, `quit`.

`readfile` blocks until the head reader reaches a milestone:
- `full` — entire file read
- `quick` — first lines available + tail at EOF
- `50%` — 50% of file by bytes read

## Dual-reader architecture

Both head and tail readers start simultaneously for every file:

- **Head reader**: reads from byte 0 forward to `tail_start` (= `max(0, EOF - 64KB)`).
  Provides the HOME view. Stops at `tail_start` to avoid duplicating tail lines.
- **Tail reader**: seeks to `tail_start`, reads forward to EOF (initial screenful),
  then follows appends. Provides the END/follow view. Shows the last ~500 lines
  immediately without reading the whole file.

Lines from both readers are merged by sorting on `byte_offset` and renumbered
sequentially. This gives correct file order regardless of which reader finishes
first.

**Line count estimation**: The tail reader computes an estimated total line
count from the average line size in its 64KB chunk (`file_size / avg_line_size`).
Shown as `~N (est!)` in yellow in the status bar until the head reader finishes
and provides the exact count.

**Status bar**: shows FOLLOW indicator, line count (with est! if estimated),
lines/sec, severity flags (EWIDT), file position percentage, and scroll position.

## Default mode

lr starts in **head mode** (showing the beginning of the file). Use `-f` or
`--follow` to start in follow/tail mode (like `tail -f`).

## Module map

- `src/main.rs` — entry point, module declarations.
- `src/cli.rs` — clap arg parsing, tracing init, tokio runtime creation.
- `src/config.rs` — `~/.config/lr/config.toml` loading.
- `src/theme.rs` — color themes.
- `src/app/` — TUI run loop, state, key event → action mapping.
- `src/io/` — dual-FD file open, head/tail/stdin readers, line splitter, line buffer.
- `src/pipeline/` — parser pipeline, line-offset index, `ParsedLine`, `Parser`.
- `src/plugin/` — `Plugin` trait, registry, Rust core plugins, Lua host.
- `src/db/` — `Storage` trait + SQLite impl.
- `src/search/` — regex/literal search.
- `src/filter/` — filter expression AST.
- `src/ui/` — ratatui rendering.

## Pipeline architecture (phase 1)

The data flow is: **readers → [raw channel] → parser → [parsed channel] → UI**.

- Head reader (`spawn_blocking`): reads file from byte 0 to initial EOF.
- Tail reader (`spawn_blocking`): polls for appends every 100ms, follows file.
- Stdin reader (`spawn_blocking`): reads stdin line-by-line.
- Parser (`tokio::spawn`): receives `RawLine`s, runs plugin chain (format
  detection → timestamp → severity), sends `ParsedLine`s.
- UI (main thread): sync event loop, drains parsed lines via `try_recv`,
  renders at ~30fps, polls keyboard with 50ms timeout.

Channels are bounded (10k capacity) for natural backpressure. The tail reader
checks `tx.is_closed()` each iteration to exit cleanly when the UI quits.
