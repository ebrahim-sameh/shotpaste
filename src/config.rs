use anyhow::{Context, Result};
use std::path::PathBuf;

/// Resolve the OS-default screenshot folder.
///
/// - Windows: `%USERPROFILE%\Pictures\Screenshots`
/// - macOS: `~/Desktop` (system default; users who relocate via
///   `defaults write com.apple.screencapture location ...` should pass `--path`)
/// - Linux: `${XDG_PICTURES_DIR:-$HOME/Pictures}/Screenshots`
#[cfg(target_os = "windows")]
pub fn default_watch_dir() -> Result<PathBuf> {
    let pictures = dirs::picture_dir().context("could not resolve Pictures directory")?;
    Ok(pictures.join("Screenshots"))
}

#[cfg(target_os = "macos")]
pub fn default_watch_dir() -> Result<PathBuf> {
    dirs::desktop_dir().context("could not resolve Desktop directory")
}

#[cfg(target_os = "linux")]
pub fn default_watch_dir() -> Result<PathBuf> {
    let pictures = dirs::picture_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Pictures")))
        .context("could not resolve Pictures directory")?;
    Ok(pictures.join("Screenshots"))
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
pub fn default_watch_dir() -> Result<PathBuf> {
    anyhow::bail!("unsupported platform — pass an explicit watch path")
}
