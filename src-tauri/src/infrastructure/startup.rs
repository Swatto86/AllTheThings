//! Manage the elevated logon scheduled task that auto-starts AllTheThings.
//! Uses `schtasks.exe`; `/rl highest` requires the caller to be elevated.

use std::os::windows::process::CommandExt;
use std::process::Command;

const TASK_NAME: &str = "AllTheThings";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Whether the logon task is currently registered.
pub fn task_exists() -> bool {
    Command::new("schtasks")
        .args(["/query", "/tn", TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Register (or replace) the task to launch `exe --minimized` at logon, elevated.
pub fn register(exe: &str) -> Result<(), String> {
    let run = format!("\"{exe}\" --minimized");
    let status = Command::new("schtasks")
        .args([
            "/create", "/tn", TASK_NAME, "/tr", &run, "/sc", "onlogon", "/rl", "highest", "/f",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("schtasks failed to create the startup task (admin required)".into())
    }
}

/// Remove the logon task. Succeeds even if it does not exist.
pub fn unregister() -> Result<(), String> {
    Command::new("schtasks")
        .args(["/delete", "/tn", TASK_NAME, "/f"])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}
