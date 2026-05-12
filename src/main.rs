// Earlier versions set `windows_subsystem = "windows"` to hide the console
// for the daemon, but that also silences CLI subcommands like `status` /
// `install` / `uninstall` because the binary has no console attached when
// invoked from a shell. The Windows installer now writes a VBS shim that
// launches the watcher via `wscript.exe` with the hidden window style, so
// the binary itself can stay a normal console subsystem and CLI output
// works as expected.
mod clipboard;
mod config;
mod installer;
mod single_instance;
mod watcher;

#[cfg(feature = "tray")]
mod notify;
#[cfg(feature = "tray")]
mod tray;
#[cfg(all(feature = "tray", target_os = "windows"))]
mod windows_shortcut;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "shotpaste",
    version,
    about = "One screenshot, three pastes — atomic multi-format clipboard.",
    long_about = "shotpaste watches a folder for new screenshot PNGs and writes each one to \
        the OS clipboard with image, file-drop, and text-path formats simultaneously, \
        so a single Ctrl+V (or Cmd+V) does the right thing in any app."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the watcher in the foreground. Writes new PNGs in the watched
    /// directory to the clipboard.
    Watch {
        /// Directory to watch. Defaults to the OS screenshot folder.
        path: Option<PathBuf>,

        /// Skip the tray UI and toasts; behave like the pre-tray CLI.
        /// Auto-enabled on Linux when no display is detected.
        #[arg(long)]
        headless: bool,
    },

    /// Register the watcher to start automatically at login.
    Install,

    /// Remove the auto-start entry. Pass --purge to also remove config.
    Uninstall {
        #[arg(long)]
        purge: bool,
    },

    /// Write a single PNG file to the clipboard with all three formats, then exit.
    /// Useful for testing without a watcher.
    Once {
        /// Path to a PNG file.
        path: PathBuf,
    },

    /// Show whether the auto-start entry exists.
    Status,
}

fn init_tracing_stderr() {
    let filter =
        EnvFilter::try_from_env("SHOTPASTE_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

/// Initialize tracing with both stderr and a rolling daily file appender
/// at `<cache>/shotpaste/shotpaste.log`. Returns a `WorkerGuard` that must
/// be kept alive for the lifetime of the process to flush the file writer.
#[cfg(feature = "tray")]
fn init_tracing_with_file() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let dir = match log_dir() {
        Some(d) => d,
        None => {
            init_tracing_stderr();
            return None;
        }
    };
    if std::fs::create_dir_all(&dir).is_err() {
        init_tracing_stderr();
        return None;
    }
    let appender = tracing_appender::rolling::daily(&dir, "shotpaste.log");
    let (nonblocking, guard) = tracing_appender::non_blocking(appender);

    let filter =
        EnvFilter::try_from_env("SHOTPASTE_LOG").unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(true)
                .with_target(false),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(nonblocking)
                .with_ansi(false)
                .with_target(false),
        )
        .init();

    Some(guard)
}

/// Directory where the rolling log lives. `<cache>/shotpaste/`.
pub fn log_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|c| c.join("shotpaste"))
}

/// Best-effort guess at "the current log file" — today's rolling file.
/// Used by the tray's "Open log file" menu item.
pub fn log_path() -> Option<PathBuf> {
    let dir = log_dir()?;
    // tracing-appender writes `<prefix>.<YYYY-MM-DD>`; we can't easily know
    // the exact filename without scanning, so just open the directory and
    // let the file manager show the user the newest entry.
    Some(dir)
}

#[cfg(target_os = "linux")]
fn display_available() -> bool {
    std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

#[cfg(not(target_os = "linux"))]
fn display_available() -> bool {
    true
}

fn run_headless(dir: &Path) -> Result<()> {
    init_tracing_stderr();
    let _guard = single_instance::acquire()?;
    watcher::run(dir, watcher::LogSink)
}

#[cfg(feature = "tray")]
fn run_with_tray(dir: &Path) -> Result<()> {
    let _log_guard = init_tracing_with_file();
    let _guard = single_instance::acquire()?;
    tray::run(dir)
}

#[cfg(not(feature = "tray"))]
fn run_with_tray(dir: &Path) -> Result<()> {
    // Compiled without tray support — fall through to headless behavior.
    run_headless(dir)
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Watch { path, headless } => {
            let dir = match path {
                Some(p) => p,
                None => config::default_watch_dir()?,
            };
            if headless || !display_available() {
                run_headless(&dir)
            } else {
                run_with_tray(&dir)
            }
        }
        Command::Install => {
            init_tracing_stderr();
            installer::install()
        }
        Command::Uninstall { purge } => {
            init_tracing_stderr();
            #[cfg(all(feature = "tray", target_os = "windows"))]
            {
                if purge {
                    windows_shortcut::remove_aumid_shortcut();
                }
            }
            installer::uninstall(purge)
        }
        Command::Once { path } => {
            init_tracing_stderr();
            clipboard::write_png(&path)
        }
        Command::Status => {
            init_tracing_stderr();
            installer::status()
        }
    }
}
