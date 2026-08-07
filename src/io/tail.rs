//! Tail-follow loop. Watches a file for appends and emits new bytes.
//!
//! Uses the `notify` crate (kqueue on macOS, inotify on Linux) with a polling
//! fallback. Stub for phase 0; full implementation in phase 1.

use std::path::PathBuf;

/// Configuration for the tail task.
pub struct TailConfig {
    pub path: PathBuf,
    /// Poll interval when watcher events aren't available.
    pub poll_interval_ms: u64,
}

impl Default for TailConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            poll_interval_ms: 250,
        }
    }
}

// TODO(phase 1): spawn_tail(config, tx) -> JoinHandle that watches the file
// and sends appended bytes on `tx`.
