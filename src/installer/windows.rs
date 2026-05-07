use anyhow::{Context, Result, bail};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tracing::info;

const TASK_NAME: &str = "Shotpaste";
const VBS_NAME: &str = "shotpaste-watch.vbs";

/// Register the watcher as a Scheduled Task that runs at login.
///
/// `Register-ScheduledTask` (via PowerShell) is used rather than
/// `schtasks.exe`: the latter returns "Access is denied" for ONLOGON
/// tasks under standard UAC even with `/RL LIMITED`, while the cmdlet
/// works for the current user without elevation.
///
/// The task's action is `wscript.exe <shim.vbs>`, not `shotpaste.exe`
/// directly, so the watcher launches with no visible console window
/// (WScript.Shell.Run intWindowStyle=0). The shim is regenerated on
/// every install so it always points at the right binary path.
pub fn install() -> Result<()> {
    let exe = env::current_exe().context("failed to determine shotpaste binary path")?;
    let exe_str = exe
        .to_str()
        .context("shotpaste path is not valid UTF-8 (rename the install dir)")?;

    // Write the VBS launcher next to the user's local-state directory.
    // It launches `<exe> watch` with WScript.Shell.Run window style 0
    // (SW_HIDE), so the watcher has no console at all.
    let vbs = vbs_path()?;
    if let Some(parent) = vbs.parent() {
        fs::create_dir_all(parent).context("failed to create shotpaste state dir")?;
    }
    let vbs_body = format!(
        "Set oShell = CreateObject(\"WScript.Shell\")\r\n\
         oShell.Run \"\"\"{exe}\"\" watch\", 0, False\r\n",
        exe = exe_str.replace('"', "\"\"")
    );
    fs::write(&vbs, vbs_body).context("failed to write VBS shim")?;
    info!(path = %vbs.display(), "wrote VBS launcher shim");

    let vbs_str = vbs
        .to_str()
        .context("VBS path is not valid UTF-8")?
        .to_string();

    let exe_ps = ps_squote("wscript.exe");
    // Argument to wscript.exe must be the VBS path quoted (in case of spaces).
    let arg_ps = ps_squote(&format!("\"{vbs_str}\""));
    let task_ps = ps_squote(TASK_NAME);

    let script = format!(
        "$ErrorActionPreference='Stop';\
         $action = New-ScheduledTaskAction -Execute {exe} -Argument {arg};\
         $trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME;\
         $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries \
            -DontStopIfGoingOnBatteries -StartWhenAvailable;\
         $task = New-ScheduledTask -Action $action -Trigger $trigger -Settings $settings;\
         Register-ScheduledTask -TaskName {task} -InputObject $task -Force | Out-Null;\
         Start-ScheduledTask -TaskName {task};",
        exe = exe_ps,
        arg = arg_ps,
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

    // Remove the VBS shim too. Best-effort — its absence is not a failure.
    if let Ok(vbs) = vbs_path()
        && vbs.exists()
    {
        let _ = fs::remove_file(&vbs);
        info!(path = %vbs.display(), "removed VBS launcher shim");
    }

    if purge {
        purge_config()?;
    }
    Ok(())
}

pub fn status() -> Result<()> {
    let task_ps = ps_squote(TASK_NAME);
    // Build the output line using string concatenation rather than `"..."`
    // interpolation, because the double quotes get stripped by powershell.exe's
    // command-line argument parser, leaving `$($t.State)` evaluated as a pipe
    // expression which fails with "Expressions are only allowed as the first
    // element of a pipeline."
    let script = format!(
        "$ErrorActionPreference='Stop';\
         try {{ \
             $t = Get-ScheduledTask -TaskName {task} -ErrorAction Stop; \
             $i = Get-ScheduledTaskInfo -TaskName {task}; \
             'REGISTERED|' + $t.State + '|' + $i.LastRunTime + '|' + $i.LastTaskResult; \
         }} catch {{ 'MISSING' }};",
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

fn vbs_path() -> Result<PathBuf> {
    let local = dirs::data_local_dir().context("failed to resolve LocalAppData")?;
    Ok(local.join("shotpaste").join(VBS_NAME))
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
