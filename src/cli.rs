//! Command-line interface.

use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use tracing_subscriber::EnvFilter;

/// LR — a fast, plugin-driven terminal log reader.
#[derive(Debug, Parser)]
#[command(name = "lr", version, about, long_about = None)]
pub struct Cli {
    /// Log files to open. If omitted, reads from stdin.
    files: Vec<PathBuf>,

    /// Read from stdin instead of (or in addition to) files.
    #[arg(short = 'i', long)]
    stdin: bool,

    /// Path to a config file. Defaults to ~/.config/lr/config.toml.
    #[arg(short = 'c', long)]
    config: Option<PathBuf>,

    /// Theme name or path to a theme file.
    #[arg(short = 't', long)]
    theme: Option<String>,

    /// Force a plugin by name (skips auto-detection).
    #[arg(short = 'p', long)]
    plugin: Option<String>,

    /// Verbosity for the debug log written to ~/.local/share/lr/debug.log.
    #[arg(short = 'v', long, default_value = "info")]
    log_level: LogLevel,

    /// Print the effective config and exit (for debugging setup).
    #[arg(long)]
    print_config: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
#[allow(non_camel_case_types)]
enum LogLevel {
    error,
    warn,
    info,
    debug,
    trace,
}

impl LogLevel {
    fn as_str(self) -> &'static str {
        match self {
            LogLevel::error => "error",
            LogLevel::warn => "warn",
            LogLevel::info => "info",
            LogLevel::debug => "debug",
            LogLevel::trace => "trace",
        }
    }
}

// Global handle to the debug log file path so we can mention it on panic.
static DEBUG_LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

impl Cli {
    pub fn parse_and_run() -> Result<()> {
        let cli = Cli::parse();
        cli.init_tracing()?;
        let rt = tokio::runtime::Runtime::new().context("create tokio runtime")?;
        rt.block_on(crate::app::run(cli))
    }

    fn init_tracing(&self) -> Result<()> {
        let dir = dirs_log_dir()?;
        std::fs::create_dir_all(&dir).context("create debug log dir")?;
        let path = dir.join("debug.log");
        DEBUG_LOG_PATH
            .set(path.clone())
            .ok()
            .context("debug log path already set")?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .context("open debug log")?;
        let filter = EnvFilter::try_new(self.log_level.as_str()).unwrap_or_else(|_| EnvFilter::new("info"));
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(file)
            .with_ansi(false)
            .try_init()
            .ok();
        tracing::info!("lr starting; debug log at {}", path.display());
        Ok(())
    }

    /// Accessor used by `app::run`.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    pub fn stdin(&self) -> bool {
        self.stdin
    }

    pub fn config_path(&self) -> Option<&PathBuf> {
        self.config.as_ref()
    }

    pub fn theme(&self) -> Option<&str> {
        self.theme.as_deref()
    }

    pub fn plugin(&self) -> Option<&str> {
        self.plugin.as_deref()
    }

    pub fn print_config(&self) -> bool {
        self.print_config
    }
}

fn dirs_log_dir() -> Result<PathBuf> {
    if let Some(h) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(h).join(".local/share/lr"));
    }
    anyhow::bail!("HOME not set; cannot determine debug log directory")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_level_as_str_round_trips() {
        assert_eq!(LogLevel::error.as_str(), "error");
        assert_eq!(LogLevel::trace.as_str(), "trace");
    }
}
