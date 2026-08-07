//! REPL / command mode. A non-TTY frontend that reads text commands from
//! stdin or a TCP connection and outputs text or JSON responses.
//!
//! See `PLAN.md` — this module enables testing, scripting, and remote control
//! without a terminal.

pub mod command;
pub mod dispatcher;
pub mod stdin;
pub mod tcp;
