use anyhow::{Context, Result, bail};
use std::env;
use std::path::PathBuf;
use std::process::Command;
use tracing::info;

const LABEL: &str = "dev.shotpaste.watcher";

pub fn install() -> Result<()> {
    let exe = env::current_exe().context("failed to determine shotpaste binary path")?;
    let exe_str = exe.to_str().context("shotpaste path is not valid UTF-8")?;

    let log = log_path()?;
    let log_str = log.to_str().context("log path is not valid UTF-8")?;

    let plist_text = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>watch</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        label = LABEL,
        exe = exe_str,
        log = log_str,
    );

    let plist = plist_path()?;
    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent).context("failed to create LaunchAgents dir")?;
    }
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent).context("failed to create Logs dir")?;
    }
    std::fs::write(&plist, plist_text).context("failed to write LaunchAgent plist")?;
    info!(path = %plist.display(), "wrote LaunchAgent plist");

    let domain = format!("gui/{}", current_uid()?);
    let plist_arg = plist
        .to_str()
        .context("plist path is not valid UTF-8")?
        .to_string();

    // Idempotent: bootout (best-effort) before bootstrap.
    let _ = Command::new("launchctl")
        .args(["bootout", &domain, &plist_arg])
        .output();

    let output = Command::new("launchctl")
        .args(["bootstrap", &domain, &plist_arg])
        .output()
        .context("failed to invoke launchctl")?;
    if !output.status.success() {
        bail!(
            "launchctl bootstrap failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    info!(label = LABEL, "loaded LaunchAgent");
    Ok(())
}

pub fn uninstall(purge: bool) -> Result<()> {
    let plist = plist_path()?;
    if plist.exists() {
        let domain = format!("gui/{}", current_uid()?);
        let plist_arg = plist
            .to_str()
            .context("plist path is not valid UTF-8")?
            .to_string();
        let _ = Command::new("launchctl")
            .args(["bootout", &domain, &plist_arg])
            .output();
        std::fs::remove_file(&plist).context("failed to remove plist")?;
        info!(path = %plist.display(), "removed LaunchAgent plist");
    }

    if purge {
        purge_config()?;
    }
    Ok(())
}

pub fn status() -> Result<()> {
    let plist = plist_path()?;
    if !plist.exists() {
        println!("shotpaste: not installed (no LaunchAgent plist).");
        println!("  run `shotpaste install` to register it");
        return Ok(());
    }

    let target = format!("gui/{}/{}", current_uid()?, LABEL);
    let output = Command::new("launchctl").args(["print", &target]).output();
    let loaded = matches!(output, Ok(o) if o.status.success());

    println!(
        "shotpaste: LaunchAgent '{LABEL}' {}.",
        if loaded { "is loaded" } else { "is NOT loaded" }
    );
    println!("  plist:   {}", plist.display());
    println!("  details: launchctl print {target}");
    Ok(())
}

fn plist_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("failed to resolve home dir")?;
    Ok(home
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn log_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("failed to resolve home dir")?;
    Ok(home.join("Library/Logs/shotpaste.log"))
}

fn current_uid() -> Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("failed to invoke `id -u`")?;
    if !output.status.success() {
        bail!("`id -u` failed");
    }
    Ok(String::from_utf8(output.stdout)
        .context("`id -u` output is not UTF-8")?
        .trim()
        .to_string())
}

fn purge_config() -> Result<()> {
    if let Some(cfg) = dirs::config_dir() {
        let path = cfg.join("shotpaste");
        if path.exists() {
            std::fs::remove_dir_all(&path).context("failed to remove config dir")?;
            info!(path = %path.display(), "purged config dir");
        }
    }
    Ok(())
}
