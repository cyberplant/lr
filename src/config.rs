//! Configuration loading and keybinding/theme resolution.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Top-level LR configuration, loaded from `~/.config/lr/config.toml` by
/// default (overridable with `--config`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub keybindings: Keybindings,
    #[serde(default)]
    pub buffer: BufferConfig,
    #[serde(default)]
    pub plugins: PluginsConfig,
    #[serde(default)]
    pub lua: LuaConfig,
    #[serde(default)]
    pub db: DbConfig,
    #[serde(default)]
    pub stdin: StdinConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Keybindings {
    // TODO(phase 6): map action -> key. Left empty for now; defaults live in
    // `app::events`.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferConfig {
    /// Maximum number of parsed lines kept in memory in the line buffer.
    #[serde(default = "default_buffer_lines")]
    pub max_lines: usize,
}

fn default_buffer_lines() -> usize {
    100_000
}

impl Default for BufferConfig {
    fn default() -> Self {
        Self {
            max_lines: default_buffer_lines(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginsConfig {
    /// Force a plugin by name, skipping auto-detection. Mirrors `--plugin`.
    #[serde(default)]
    pub force: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LuaConfig {
    /// If true, Lua plugins get access to `os`/`io`/`require`. Off by default
    /// for safety.
    #[serde(default)]
    pub unsafe_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbConfig {
    /// Maximum rows retained in the in-memory SQLite database before oldest
    /// rows are evicted. 0 = unbounded.
    #[serde(default = "default_db_max_rows")]
    pub max_rows: usize,
}

fn default_db_max_rows() -> usize {
    1_000_000
}

impl Default for DbConfig {
    fn default() -> Self {
        Self {
            max_rows: default_db_max_rows(),
        }
    }
}

/// Controls how stdin data is handled. Stdin cannot be seeked, so lr must
/// either spill it to a temp file (enabling full random access) or keep it
/// in a bounded ring buffer (discarding old data).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StdinConfig {
    /// Strategy for handling stdin data:
    /// - "temp-file" (default): spill stdin to a temp file, then use the
    ///   same file-backed LineStore as regular files. Full random access.
    /// - "memory": keep stdin in a bounded in-memory ring buffer. Old data
    ///   is discarded when the buffer exceeds `memory_limit_mb`.
    #[serde(default = "default_stdin_mode")]
    pub mode: StdinMode,

    /// Memory limit in MB for the "memory" mode ring buffer.
    /// Ignored in "temp-file" mode. Default: 1024 (1 GB).
    #[serde(default = "default_stdin_memory_limit_mb")]
    pub memory_limit_mb: usize,
}

fn default_stdin_mode() -> StdinMode {
    StdinMode::TempFile
}

fn default_stdin_memory_limit_mb() -> usize {
    1024
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum StdinMode {
    TempFile,
    Memory,
}

impl Default for StdinConfig {
    fn default() -> Self {
        Self {
            mode: default_stdin_mode(),
            memory_limit_mb: default_stdin_memory_limit_mb(),
        }
    }
}

impl Config {
    /// Load config from the explicit path, or the default user config path.
    /// Missing default config is not an error — we fall back to `Config::default()`.
    pub fn load(explicit: Option<&Path>) -> Result<Config> {
        let path = match explicit {
            Some(p) => Some(p.to_path_buf()),
            None => default_config_path(),
        };
        match path {
            Some(p) if p.exists() => {
                let raw = std::fs::read_to_string(&p)
                    .with_context(|| format!("read config {}", p.display()))?;
                let cfg: Config = toml::from_str(&raw)
                    .with_context(|| format!("parse config {}", p.display()))?;
                Ok(cfg)
            }
            _ => Ok(Config::default()),
        }
    }
}

fn default_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/lr/config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_round_trips_toml() {
        let cfg = Config::default();
        let s = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back.buffer.max_lines, cfg.buffer.max_lines);
        assert!(!back.lua.unsafe_enabled);
    }

    #[test]
    fn missing_file_is_default() {
        let cfg = Config::load(Some(Path::new("/nonexistent/lr/config.toml"))).unwrap();
        assert_eq!(cfg.buffer.max_lines, 100_000);
    }
}
