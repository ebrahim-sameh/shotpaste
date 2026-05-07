use anyhow::{Context, Result, bail};
use std::env;
use std::process::Command;
use tracing::info;

const TASK_NAME: &str = "Shotpaste";

/// Drive Scheduled Task registration through PowerShell's
/// `Register-ScheduledTask` rather than `schtasks.exe`. Both ultimately call
/// the same Task Scheduler COM API, but `schtasks.exe` returns "Access is
/// denied" for ONLOGON tasks under standard UAC even with `/RL LIMITED`,
/// while `Register-ScheduledTask` works for the current user without elevation.
pub fn install() -> Result<()> {
    let exe = env::current_exe().context("failed to determine shotpaste binary path")?;
    let exe_str = exe
        .to_str()
        .context("shotpaste path is not valid UTF-8 (rename the install dir)")?;

    // PowerShell single-quoted strings escape ' as ''. Apply to both the exe
    // path and the task name so a path like `C:\foo's bar\` survives intact.
    let exe_ps = ps_squote(exe_str);
    let task_ps = ps_squote(TASK_NAME);

    let script = format!(
        "$ErrorActionPreference='Stop';\
         $action = New-ScheduledTaskAction -Execute {exe} -Argument 'watch';\
         $trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME;\
         $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries \
            -DontStopIfGoingOnBatteries -StartWhenAvailable;\
         $task = New-ScheduledTask -Action $action -Trigger $trigger -Settings $settings;\
         Register-ScheduledTask -TaskName {task} -InputObject $task -Force | Out-Null;\
         Start-ScheduledTask -TaskName {task};",
        exe = exe_ps,
        task = task_ps,
    );

    let output = run_powershell(&script).context("failed to invoke PowerShell")?;
    if !output.status.success() {
        bail!(
            "Register-ScheduledTask failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    info!(task = TASK_NAME, exe = %exe.display(), "registered & started Scheduled Task");
    Ok(())
}

pub fn uninstall(purge: bool) -> Result<()> {
    let task_ps = ps_squote(TASK_NAME);
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue';\
         try {{ Stop-ScheduledTask -TaskName {task} }} catch {{}};\
         Unregister-ScheduledTask -TaskName {task} -Confirm:$false;",
        task = task_ps,
    );

    let output = run_powershell(&script).context("failed to invoke PowerShell")?;
    // PowerShell's SilentlyContinue swallows "task not found"; we don't
    // distinguish "removed" vs "wasn't registered" — both end up with no task.
    let _ = output;
    info!(
        task = TASK_NAME,
        "Scheduled Task removed (or was already absent)"
    );

    if purge {
        purge_config()?;
    }
    Ok(())
}

pub fn status() -> Result<()> {
    let task_ps = ps_squote(TASK_NAME);
    let script = format!(
        "$ErrorActionPreference='Stop';\
         try {{ \
             $t = Get-ScheduledTask -TaskName {task} -ErrorAction Stop; \
             $i = Get-ScheduledTaskInfo -TaskName {task}; \
             Write-Output \"REGISTERED|$($t.State)|$($i.LastRunTime)|$($i.LastTaskResult)\"; \
         }} catch {{ Write-Output 'MISSING' }};",
        task = task_ps,
    );

    let output = run_powershell(&script).context("failed to invoke PowerShell")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().last().unwrap_or("").trim();

    if line == "MISSING" {
        println!("shotpaste: not installed (Scheduled Task '{TASK_NAME}' missing).");
        println!("  run `shotpaste install` to register it");
    } else if let Some(rest) = line.strip_prefix("REGISTERED|") {
        let parts: Vec<&str> = rest.split('|').collect();
        let state = parts.first().copied().unwrap_or("?");
        let last_run = parts.get(1).copied().unwrap_or("?");
        let last_result = parts.get(2).copied().unwrap_or("?");
        println!("shotpaste: Scheduled Task '{TASK_NAME}' is registered.");
        println!("  state:       {state}");
        println!("  last run:    {last_run}");
        println!("  last result: {last_result}");
    } else {
        println!("shotpaste: status check returned unexpected output: {line}");
    }
    Ok(())
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

/// Wrap a string in single quotes for safe PowerShell embedding,
/// doubling any internal `'` per PS quoting rules.
fn ps_squote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
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
