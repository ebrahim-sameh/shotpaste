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

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::warn;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "shotpaste",
    version,
    about = "One screenshot, three pastes — atomic multi-format clipboard.",
    long_about = "shotpaste watches one or more folders for new screenshot PNGs and writes \
        each one to the OS clipboard with image, file-drop, and text-path formats \
        simultaneously, so a single Ctrl+V (or Cmd+V) does the right thing in any app."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the watcher in the foreground. Writes new PNGs in any of the
    /// watched directories to the clipboard.
    Watch {
        /// Directories to watch. With no args, uses `watch_dirs` from
        /// config.toml, or the OS default screenshot folder if config is
        /// empty. Pass one or more paths to override for this invocation.
        #[arg(num_args = 0..)]
        paths: Vec<PathBuf>,

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

/// Resolve the effective watch-dir list. Precedence:
///   1. CLI positional args (if any).
///   2. `cfg.watch_dirs` from config.toml (if non-empty).
///   3. `[config::default_watch_dir()?]` — single-element fallback.
///
/// Canonicalizes when possible (preserves the user's original path if
/// canonicalize fails, so non-existent dirs still pass through to the
/// watcher which decides whether to skip them) and dedups.
#[cfg_attr(not(feature = "tray"), allow(dead_code))]
fn resolve_watch_dirs(cli: &[PathBuf], cfg: &config::Config) -> Result<Vec<PathBuf>> {
    let source: Vec<PathBuf> = if !cli.is_empty() {
        cli.to_vec()
    } else if !cfg.watch_dirs.is_empty() {
        cfg.watch_dirs.clone()
    } else {
        vec![config::default_watch_dir().context("could not resolve default watch dir")?]
    };

    let mut out: Vec<PathBuf> = Vec::with_capacity(source.len());
    for p in source {
        let resolved = std::fs::canonicalize(&p).unwrap_or(p);
        if !out.iter().any(|existing| existing == &resolved) {
            out.push(resolved);
        }
    }
    Ok(out)
}

/// Auto-create only when the watcher would otherwise run with zero dirs.
/// Used for the single-default fallback so first-run UX still works on a
/// fresh machine where `~/Pictures/Screenshots` doesn't exist yet.
fn ensure_default_dir_exists(dir: &std::path::Path) {
    if !dir.exists() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            warn!(dir = %dir.display(), error = %e, "could not create default watch dir");
        }
    }
}

fn run_headless(dirs: &[PathBuf]) -> Result<()> {
    init_tracing_stderr();
    let _guard = single_instance::acquire()?;
    watcher::run(dirs, watcher::LogSink)
}

#[cfg(feature = "tray")]
fn run_with_tray(dirs: Vec<PathBuf>) -> Result<()> {
    let _log_guard = init_tracing_with_file();
    let _guard = single_instance::acquire()?;
    tray::run(dirs)
}

#[cfg(not(feature = "tray"))]
fn run_with_tray(dirs: Vec<PathBuf>) -> Result<()> {
    run_headless(&dirs)
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Watch { paths, headless } => {
            #[cfg(feature = "tray")]
            let cfg = config::Config::load();
            #[cfg(not(feature = "tray"))]
            let cfg = config::Config::default();

            let dirs = resolve_watch_dirs(&paths, &cfg)?;

            // First-run UX: if we're using the single default-fallback dir,
            // auto-create it so `shotpaste install`-then-screenshot just
            // works on a fresh machine.
            if paths.is_empty() && cfg.watch_dirs.is_empty() && dirs.len() == 1 {
                ensure_default_dir_exists(&dirs[0]);
            }

            if headless || !display_available() {
                run_headless(&dirs)
            } else {
                run_with_tray(dirs)
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
