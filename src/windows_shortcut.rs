//! Windows-only: register an AppUserModelID so toast notifications display
//! "shotpaste" as the sender instead of the "Windows PowerShell" default
//! that `winrt-notification` falls back to when no AUMID is known.
//!
//! Approach: write `HKEY_CURRENT_USER\Software\Classes\AppUserModelId\<aumid>`
//! with `DisplayName` and `IconUri` values. This is Microsoft's documented
//! unpackaged-app registration path and is dramatically simpler than the
//! Start-Menu-shortcut + `IPropertyStore` dance (which requires embedding
//! a C# COM wrapper via `Add-Type`).
//!
//! Idempotent: it's just three registry values per call, ~10ms total.

#![cfg(target_os = "windows")]

use anyhow::{Context, Result, bail};
use std::env;
use std::process::Command;
use tracing::info;

/// Ensure HKCU\Software\Classes\AppUserModelId\<aumid> exists with a
/// human-friendly DisplayName and an IconUri pointing at the running
/// shotpaste binary. Safe to call repeatedly.
pub fn ensure_aumid_shortcut(aumid: &str) -> Result<()> {
    let exe = env::current_exe().context("failed to determine shotpaste binary path")?;
    let exe_str = exe
        .to_str()
        .context("shotpaste path is not valid UTF-8 (rename the install dir)")?;

    let aumid_ps = ps_squote(aumid);
    let icon_ps = ps_squote(exe_str);

    // `New-Item -Force` creates the key (idempotent), `New-ItemProperty -Force`
    // overwrites the value. SilentlyContinue swallows the noisy "already exists"
    // errors from re-runs.
    let script = format!(
        "$ErrorActionPreference='Stop';\
         $key = 'HKCU:\\Software\\Classes\\AppUserModelId\\' + {aumid};\
         if (-not (Test-Path $key)) {{ New-Item -Path $key -Force | Out-Null }};\
         New-ItemProperty -Path $key -Name 'DisplayName' -Value 'shotpaste' \
            -PropertyType String -Force | Out-Null;\
         New-ItemProperty -Path $key -Name 'IconUri' -Value {icon} \
            -PropertyType String -Force | Out-Null;",
        aumid = aumid_ps,
        icon = icon_ps,
    );

    let output = run_powershell(&script).context("failed to invoke PowerShell")?;
    if !output.status.success() {
        bail!(
            "AUMID registration failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    info!(aumid, "registered AUMID for branded toasts");
    Ok(())
}

/// Remove the AUMID registry key. Best-effort — invoked from
/// `uninstall --purge`. Missing key is not an error.
pub fn remove_aumid_shortcut() {
    let script = "$ErrorActionPreference='SilentlyContinue';\
         Remove-Item -Path 'HKCU:\\Software\\Classes\\AppUserModelId\\dev.shotpaste.watcher' \
            -Recurse -Force;"
        .to_string();
    let _ = run_powershell(&script);
    info!("removed AUMID registry entry (if present)");
}

fn run_powershell(script: &str) -> std::io::Result<std::process::Output> {
    Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .output()
}

fn ps_squote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}
