//! Manage the logon scheduled task that auto-launches AllTheThings into the tray
//! at sign-in. Uses `schtasks.exe`. Since the GUI now runs unelevated and the
//! background service does the indexing, the task launches the GUI **without**
//! elevation (no `/rl highest`) — so the auto-started instance shares the same
//! integrity level as a manual launch (keeping single-instance focus working)
//! and never silently re-elevates. Creating/removing a task under the root
//! folder still needs admin, so callers relaunch elevated when they aren't.

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

/// Register (or replace) the task to launch `exe --minimized` (unelevated) at logon.
pub fn register(exe: &str) -> Result<(), String> {
    let run = format!("\"{exe}\" --minimized");
    let status = Command::new("schtasks")
        .args([
            "/create", "/tn", TASK_NAME, "/tr", &run, "/sc", "onlogon", "/f",
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
