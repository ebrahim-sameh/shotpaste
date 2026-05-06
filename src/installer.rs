use anyhow::{Result, bail};
use tracing::info;

/// Register the watcher daemon to start at login.
///
/// Phase A: stub. The real implementations land in Phase C:
/// - Windows: Scheduled Task `Shotpaste`, AtLogOn, Limited.
/// - macOS: `~/Library/LaunchAgents/dev.shotpaste.plist` + `launchctl bootstrap`.
/// - Linux: `~/.config/systemd/user/shotpaste.service` + `systemctl --user enable --now`.
pub fn install() -> Result<()> {
    info!(
        platform = std::env::consts::OS,
        "install: not yet implemented"
    );
    bail!("`shotpaste install` is not yet implemented (will land in Phase C)")
}

pub fn uninstall(_purge: bool) -> Result<()> {
    info!(
        platform = std::env::consts::OS,
        "uninstall: not yet implemented"
    );
    bail!("`shotpaste uninstall` is not yet implemented (will land in Phase C)")
}

pub fn status() -> Result<()> {
    println!(
        "shotpaste status: auto-start integration not yet implemented on {} (Phase C).",
        std::env::consts::OS
    );
    Ok(())
}
