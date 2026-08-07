//! LR — Log Reader.
//!
//! A fast, plugin-driven terminal log reader. See `PLAN.md` for the full
//! architecture and roadmap.

// Phase 0 scaffolding: many modules/functions are intentionally unused until
// later phases wire them up. Allow dead code at the crate level for now and
// tighten this once the pipeline is connected.
#![allow(dead_code)]

use std::process::ExitCode;

mod app;
mod cli;
mod config;
mod db;
mod filter;
mod io;
mod pipeline;
mod plugin;
mod search;
mod theme;
mod ui;

fn main() -> ExitCode {
    match cli::Cli::parse_and_run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Terminal may or may not be in raw mode; restore defensively.
            let _ = crossterm::terminal::disable_raw_mode();
            eprintln!("lr: {err:#}");
            ExitCode::FAILURE
        }
    }
}
