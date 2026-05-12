use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::warn;

/// User-tunable settings persisted to `<config_dir>/shotpaste/config.toml`.
/// Loaded once at startup; the tray writes back when a check item flips.
/// Missing or corrupt files quietly fall back to `Config::default()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Show a toast when a screenshot is pushed to the clipboard.
    /// Bursts coalesce within a short window — see `tray::ToastState`.
    #[serde(default = "default_true")]
    pub notify_on_success: bool,
    /// Show a toast when a push fails (clipboard locked, decode error, etc.).
    /// Errors never coalesce.
    #[serde(default = "default_true")]
    pub notify_on_error: bool,
    /// Override the OS-default screenshot folder. The `--path` arg on
    /// `shotpaste watch <path>` still wins over this.
    #[serde(default)]
    pub watch_dir: Option<PathBuf>,
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            notify_on_success: true,
            notify_on_error: true,
            watch_dir: None,
        }
    }
}

// `load` / `save` / `config_path` are only called by the tray module; mark
// them so `--no-default-features` builds (no tray) don't warn about them.
#[allow(dead_code)]
impl Config {
    /// Read config.toml, returning defaults if it's missing or unparseable.
    /// Parse errors log a warning but don't fail startup — we'd rather run
    /// with defaults than refuse to start because the user hand-edited the
    /// file into a bad state.
    pub fn load() -> Self {
        let path = match config_path() {
            Ok(p) => p,
            Err(_) => return Self::default(),
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "config.toml is malformed; using defaults");
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "could not read config.toml; using defaults");
                Self::default()
            }
        }
    }

    /// Write the current settings to disk. Best-effort — failures log
    /// at `warn` so a read-only filesystem doesn't crash the tray.
    pub fn save(&self) -> Result<()> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("failed to serialize config")?;
        std::fs::write(&path, text)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }
}

#[allow(dead_code)]
pub fn config_path() -> Result<PathBuf> {
    let dir = dirs::config_dir().context("could not resolve config dir")?;
    Ok(dir.join("shotpaste").join("config.toml"))
}

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
