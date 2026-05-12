//! Toast notifications.
//!
//! Thin wrapper over `notify-rust`. On Windows we set an AppUserModelID so
//! toasts are branded "shotpaste" instead of "Windows PowerShell" — that
//! requires a Start Menu shortcut bearing the same AUMID, which we create
//! lazily on first use (see [`crate::windows_shortcut`]).

use std::path::Path;
use std::sync::Once;
use tracing::warn;

/// The Application User Model ID we register with Windows for branded
/// toasts. Reverse-DNS form, kept stable so we don't orphan old shortcuts.
/// Unused on macOS/Linux but kept cross-platform so the `#[cfg]` in `show`
/// stays a single-line gate.
#[allow(dead_code)]
pub const AUMID: &str = "dev.shotpaste.watcher";

static INIT_ONCE: Once = Once::new();

/// Run platform-specific one-time setup. On Windows, ensure the
/// AUMID-bearing Start Menu shortcut exists so toasts look right.
fn ensure_init() {
    INIT_ONCE.call_once(|| {
        #[cfg(target_os = "windows")]
        {
            if let Err(e) = crate::windows_shortcut::ensure_aumid_shortcut(AUMID) {
                warn!(
                    "could not create AUMID shortcut; toasts may appear under \
                     a generic sender name ({e:#})"
                );
            }
        }
    });
}

/// Toast emitted on a single successful push.
pub fn success_single(path: &Path) {
    ensure_init();
    let body = format!(
        "Pushed image + file + path to clipboard\n{}",
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    );
    show("Screenshot copied", &body);
}

/// Toast emitted when several pushes coalesce inside the throttle window.
pub fn success_burst(n: usize) {
    ensure_init();
    let body = format!("Pushed {n} screenshots to the clipboard");
    show("Screenshots copied", &body);
}

/// Toast emitted on a failed push. Errors never coalesce.
pub fn error(path: &Path, err: &str) {
    ensure_init();
    let title = "shotpaste — push failed";
    let body = format!(
        "{}\n{}",
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        truncate(err, 240)
    );
    show(title, &body);
}

fn show(title: &str, body: &str) {
    let mut n = notify_rust::Notification::new();
    n.summary(title).body(body);
    #[cfg(target_os = "windows")]
    {
        n.app_id(AUMID);
    }
    if let Err(e) = n.show() {
        warn!("failed to show toast: {e}");
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}
