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
