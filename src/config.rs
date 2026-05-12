use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::warn;

/// User-tunable settings persisted to `<config_dir>/shotpaste/config.toml`.
/// Loaded once at startup; the tray writes back when a check item flips or
/// the watched-folder list changes. Missing or corrupt files quietly fall
/// back to `Config::default()`.
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
    /// Folders to watch for new PNG screenshots. Empty = use the OS default
    /// (see `default_watch_dir`). The `shotpaste watch <p1> <p2> …` CLI
    /// args, when supplied, override this for that invocation only.
    #[serde(default)]
    pub watch_dirs: Vec<PathBuf>,
    /// Legacy single-folder field. Kept for one release so v0.2.0 configs
    /// migrate transparently — `load()` folds any present value into
    /// `watch_dirs`. Skipped during serialization once it's `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
            watch_dirs: Vec::new(),
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
        let mut cfg = match std::fs::read_to_string(&path) {
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
        };
        cfg.migrate_legacy_watch_dir();
        cfg
    }

    /// Fold the legacy `watch_dir` field into `watch_dirs` if present, then
    /// clear it so the next `save()` writes the new schema only.
    fn migrate_legacy_watch_dir(&mut self) {
        if let Some(legacy) = self.watch_dir.take()
            && !self.watch_dirs.iter().any(|p| p == &legacy)
        {
            self.watch_dirs.push(legacy);
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
