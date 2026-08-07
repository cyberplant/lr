//! Command-line interface.

use std::io::IsTerminal;
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

    /// Follow mode: start at the end and tail for new lines (like tail -f).
    /// Without this flag, lr starts at the beginning of the file (head mode).
    #[arg(short = 'f', long)]
    follow: bool,

    /// Verbosity for the debug log written to ~/.local/share/lr/debug.log.
    #[arg(short = 'v', long, default_value = "info")]
    log_level: LogLevel,

    /// Force REPL (command) mode even if stdout is a TTY.
    /// REPL mode reads text commands from stdin and prints text/JSON output.
    /// Auto-activated when stdout is not a TTY.
    #[arg(long)]
    repl: bool,

    /// Start a TCP command server on the given address (e.g. 127.0.0.1:9999).
    /// Implies non-TTY mode. Can be combined with --repl for stdin + TCP.
    #[arg(long)]
    listen: Option<String>,

    /// Output JSON instead of text in REPL/TCP mode.
    #[arg(long)]
    json: bool,

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

    pub fn follow(&self) -> bool {
        self.follow
    }

    pub fn print_config(&self) -> bool {
        self.print_config
    }

    pub fn repl(&self) -> bool {
        self.repl
    }

    pub fn listen(&self) -> Option<&str> {
        self.listen.as_deref()
    }

    pub fn json(&self) -> bool {
        self.json
    }

    /// Returns true if the app should run in REPL/TCP mode instead of TUI.
    /// This is when --repl is given, --listen is given, or stdout is not a TTY.
    pub fn should_use_repl_mode(&self) -> bool {
        self.repl || self.listen.is_some() || !std::io::stdout().is_terminal()
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
