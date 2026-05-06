// Hide the console window on Windows release builds. Dev builds keep stdio
// for `tracing` output. The watcher daemon never needs a console.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod clipboard;
mod config;
mod installer;
mod watcher;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
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

fn init_tracing() {
    let filter =
        EnvFilter::try_from_env("SHOTPASTE_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    match cli.command {
        Command::Watch { path } => {
            let dir = match path {
                Some(p) => p,
                None => config::default_watch_dir()?,
            };
            watcher::run(&dir)
        }
        Command::Install => installer::install(),
        Command::Uninstall { purge } => installer::uninstall(purge),
        Command::Once { path } => clipboard::write_png(&path),
        Command::Status => installer::status(),
    }
}
