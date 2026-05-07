use anyhow::{Context, Result, bail};
use std::env;
use std::path::PathBuf;
use std::process::Command;
use tracing::info;

const UNIT_NAME: &str = "shotpaste.service";

pub fn install() -> Result<()> {
    let exe = env::current_exe().context("failed to determine shotpaste binary path")?;
    let exe_str = exe.to_str().context("shotpaste path is not valid UTF-8")?;

    let unit_text = format!(
        r#"[Unit]
Description=shotpaste — atomic multi-format clipboard for screenshots
After=graphical-session.target
PartOf=graphical-session.target

[Service]
Type=simple
ExecStart={exe} watch
Restart=on-failure
RestartSec=5

[Install]
WantedBy=graphical-session.target
"#,
        exe = exe_str,
    );

    let path = unit_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("failed to create systemd user dir")?;
    }
    std::fs::write(&path, unit_text).context("failed to write systemd unit")?;
    info!(path = %path.display(), "wrote systemd user unit");

    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();

    let output = Command::new("systemctl")
        .args(["--user", "enable", "--now", UNIT_NAME])
        .output()
        .context("failed to invoke systemctl")?;
    if !output.status.success() {
        bail!(
            "`systemctl --user enable --now {}` failed: {}",
            UNIT_NAME,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    info!(unit = UNIT_NAME, "enabled and started");
    Ok(())
}

pub fn uninstall(purge: bool) -> Result<()> {
    let _ = Command::new("systemctl")
        .args(["--user", "disable", "--now", UNIT_NAME])
        .output();

    let path = unit_path()?;
    if path.exists() {
        std::fs::remove_file(&path).context("failed to remove systemd unit")?;
        info!(path = %path.display(), "removed systemd unit");
    }
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();

    if purge {
        purge_config()?;
    }
    Ok(())
}

pub fn status() -> Result<()> {
    let path = unit_path()?;
    if !path.exists() {
        println!(
            "shotpaste: not installed (no systemd unit at {}).",
            path.display()
        );
        println!("  run `shotpaste install` to register it");
        return Ok(());
    }

    let active = run_systemctl(&["is-active", UNIT_NAME]);
    let enabled = run_systemctl(&["is-enabled", UNIT_NAME]);

    println!("shotpaste: systemd user unit '{UNIT_NAME}'");
    println!("  active:  {active}");
    println!("  enabled: {enabled}");
    println!("  unit:    {}", path.display());
    println!("  details: systemctl --user status {UNIT_NAME}");
    Ok(())
}

fn unit_path() -> Result<PathBuf> {
    let cfg = dirs::config_dir().context("failed to resolve config dir")?;
    Ok(cfg.join("systemd/user").join(UNIT_NAME))
}

fn run_systemctl(extra_args: &[&str]) -> String {
    let mut args = vec!["--user"];
    args.extend_from_slice(extra_args);
    match Command::new("systemctl").args(args).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => "<systemctl unavailable>".to_string(),
    }
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
