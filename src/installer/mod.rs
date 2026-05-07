//! Per-platform autostart registration.
//!
//! - Windows: Scheduled Task `Shotpaste`, AtLogOn, run level Limited.
//! - macOS:   `~/Library/LaunchAgents/dev.shotpaste.watcher.plist` loaded
//!   via `launchctl bootstrap gui/<uid>`.
//! - Linux:   `~/.config/systemd/user/shotpaste.service`, started with
//!   `systemctl --user enable --now`.
//!
//! Each implementation re-points the autostart action at
//! `std::env::current_exe()` — so installing from `~/.local/bin/shotpaste`
//! and re-running `install` from `target/release/shotpaste` correctly
//! switches the registered path.

use anyhow::Result;

#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod imp;

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod imp;

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod imp;

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
mod imp {
    use anyhow::{Result, bail};
    pub fn install() -> Result<()> {
        bail!("`shotpaste install` is not implemented on this platform")
    }
    pub fn uninstall(_purge: bool) -> Result<()> {
        bail!("`shotpaste uninstall` is not implemented on this platform")
    }
    pub fn status() -> Result<()> {
        bail!("`shotpaste status` is not implemented on this platform")
    }
}

pub fn install() -> Result<()> {
    imp::install()
}

pub fn uninstall(purge: bool) -> Result<()> {
    imp::uninstall(purge)
}

pub fn status() -> Result<()> {
    imp::status()
}
