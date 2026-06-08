//! Tauri command handlers — the only place the UI touches the backend.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::os::windows::process::CommandExt;
use std::process::Command;

use tauri::{Emitter, State};
use tauri_plugin_opener::OpenerExt;

use crate::application::export::{self, ExportFormat};
use crate::application::{IndexStatus, SearchOptions, SearchResult};
use crate::infrastructure::fileops::{self, ShellVerb};
use crate::infrastructure::service::scm::{self, SvcState};
use crate::infrastructure::{elevation, icons, startup};

use super::settings::{self, Settings, SettingsState, StartFlags};
use super::state::AppState;

/// Search via the active backend (the service, or the in-process index).
#[tauri::command]
pub fn search(state: State<'_, AppState>, options: SearchOptions) -> SearchResult {
    state.search(&options)
}

/// Report indexing progress / readiness for the status bar.
#[tauri::command]
pub fn index_status(state: State<'_, AppState>) -> IndexStatus {
    state.status()
}

/// Whether searches are served by the background service (vs. in-process), for
/// the status indicator.
#[tauri::command]
pub fn uses_service(state: State<'_, AppState>) -> bool {
    state.uses_service()
}

/// Upper bound on rows written by a single export — a guardrail, not a normal limit.
const EXPORT_CAP: usize = 1_000_000;

/// Outcome of an export: rows actually `written` and the `total` that matched.
/// They differ only when the match count exceeds [`EXPORT_CAP`], letting the UI
/// flag a capped (incomplete) export instead of reporting it as complete.
#[derive(serde::Serialize)]
pub struct ExportSummary {
    pub written: usize,
    pub total: usize,
}

/// Export the current results to `path` as `format` (`csv`/`txt`/`efu`), re-running
/// the search unbounded (up to [`EXPORT_CAP`]).
#[tauri::command]
pub fn export_results(
    state: State<'_, AppState>,
    options: SearchOptions,
    format: String,
    path: String,
) -> Result<ExportSummary, String> {
    let fmt = ExportFormat::parse(&format).ok_or_else(|| format!("unknown format '{format}'"))?;
    let mut opts = options;
    opts.limit = EXPORT_CAP;

    let result = state.search(&opts);
    if let Some(e) = result.error {
        return Err(e);
    }

    let file = File::create(&path).map_err(|e| e.to_string())?;
    let mut writer = BufWriter::new(file);
    export::write_export(&result.hits, fmt, &mut writer).map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())?;
    Ok(ExportSummary {
        written: result.hits.len(),
        total: result.total,
    })
}

/// Open a file or folder with its default handler.
#[tauri::command]
pub fn open_path(app: tauri::AppHandle, path: String) -> Result<(), String> {
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())
}

/// The shell icon for a file extension (or folder), as a base64 PNG.
#[tauri::command]
pub fn file_icon(ext: Option<String>, is_dir: bool) -> Option<String> {
    icons::icon_base64(ext.as_deref(), is_dir)
}

/// The registry's friendly type name for a file extension (or folder).
#[tauri::command]
pub fn file_type(ext: Option<String>, is_dir: bool) -> Option<String> {
    icons::type_name(ext.as_deref(), is_dir)
}

/// Current user settings, with `run_at_startup` reconciled against the real task.
#[tauri::command]
pub fn get_settings(state: State<'_, SettingsState>) -> Settings {
    let mut settings = state.0.read().clone();
    settings.run_at_startup = startup::task_exists();
    settings
}

/// Persist settings and apply side effects (register/unregister the logon task).
#[tauri::command]
pub fn set_settings(state: State<'_, SettingsState>, settings: Settings) -> Result<(), String> {
    // The logon task uses `/rl highest`, which needs admin to create or delete.
    // Only touch it when the toggle actually changed (reconciled from the real
    // task), and relaunch elevated when the GUI isn't — mirroring the service
    // commands — so an unelevated GUI can still manage it.
    if settings.run_at_startup != startup::task_exists() {
        if elevation::is_elevated() {
            apply_startup_task(settings.run_at_startup)?;
        } else {
            let arg = if settings.run_at_startup {
                "--task-install"
            } else {
                "--task-uninstall"
            };
            elevation::run_elevated(arg)?;
        }
    }
    *state.0.write() = settings.clone();
    settings::save(&settings).map_err(|e| e.to_string())?;
    Ok(())
}

/// Register or unregister the elevated logon task directly (caller is elevated).
fn apply_startup_task(enable: bool) -> Result<(), String> {
    if enable {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        startup::register(&exe.to_string_lossy())
    } else {
        startup::unregister()
    }
}

/// Whether the app was launched with `--minimized` (so the UI stays hidden).
#[tauri::command]
pub fn start_hidden(flags: State<'_, StartFlags>) -> bool {
    flags.start_hidden
}

/// Rename the item to `new_name` within its directory; returns the new path.
#[tauri::command]
pub fn rename_path(path: String, new_name: String) -> Result<String, String> {
    fileops::rename(&path, &new_name)
}

/// Move the item to the Recycle Bin (the UI confirms beforehand).
#[tauri::command]
pub fn delete_path(path: String) -> Result<(), String> {
    fileops::recycle(&path)
}

/// Invoke a shell verb (`properties` / `open_with` / `run_as`) on the item.
///
/// The dialog it opens needs the UI thread's message pump, so the verb is run on
/// the main thread. This command returns once that work is *dispatched*; a verb
/// that then fails (other than the user cancelling) is reported to the UI via a
/// `shell-error` event rather than as the command's own error.
#[tauri::command]
pub fn shell_action(app: tauri::AppHandle, path: String, action: String) -> Result<(), String> {
    let verb = ShellVerb::parse(&action).ok_or_else(|| format!("unknown action '{action}'"))?;
    let emitter = app.clone();
    app.run_on_main_thread(move || {
        if let Err(e) = fileops::shell_verb(&path, verb) {
            let _ = emitter.emit("shell-error", e);
        }
    })
    .map_err(|e| e.to_string())
}

/// Open Explorer with the item selected (its containing folder, highlighted).
#[tauri::command]
pub fn reveal_path(path: String) -> Result<(), String> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{path}\""))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ---- Background service management ----
//
// Status is read live from the SCM (never cached), so the Settings UI reconciles
// to reality — the way `get_settings` reconciles `run_at_startup` from the real
// logon task. Status needs only CONNECT + QUERY_STATUS, which an unelevated GUI
// has; install/start/stop/uninstall need admin, so they run directly when the
// GUI is already elevated (e.g. launched from the logon task) and otherwise
// relaunch this exe elevated (a UAC prompt) to do the work.

/// The service's current SCM state, for the Settings status row.
#[tauri::command]
pub fn service_status() -> Result<SvcState, String> {
    scm::status()
}

/// Register the service (LocalSystem, auto-start, this exe + `--service`).
#[tauri::command]
pub fn install_service() -> Result<(), String> {
    manage_service("--svc-install", scm::install)
}

/// Stop (if running) and remove the service registration.
#[tauri::command]
pub fn uninstall_service() -> Result<(), String> {
    manage_service("--svc-uninstall", scm::uninstall)
}

/// Start the installed service.
#[tauri::command]
pub fn start_service() -> Result<(), String> {
    manage_service("--svc-start", scm::start)
}

/// Stop the running service.
#[tauri::command]
pub fn stop_service() -> Result<(), String> {
    manage_service("--svc-stop", scm::stop)
}

/// Install **and** start the service in one elevated step (one UAC prompt),
/// used by the migration banner so adopting the service isn't two prompts.
#[tauri::command]
pub fn setup_service() -> Result<(), String> {
    manage_service("--svc-setup", setup_service_direct)
}

fn setup_service_direct() -> Result<(), String> {
    scm::install()?;
    scm::start()
}

/// Record that the one-time service-migration prompt has been shown. The
/// frontend calls this when it actually renders the banner, so a lost or
/// too-early event never permanently suppresses the nudge.
#[tauri::command]
pub fn mark_service_prompt_seen() {
    settings::mark_service_prompt_seen();
}

/// Whether the GUI is currently elevated — lets the frontend tailor its wording
/// (service actions prompt for admin only when it isn't).
#[tauri::command]
pub fn is_elevated() -> bool {
    elevation::is_elevated()
}

/// Run a service-management action directly if already elevated, otherwise
/// relaunch this exe elevated (UAC) to run the matching one-shot `--svc-*`
/// command. `direct` and `elevated_arg` must be two routes to the same action.
fn manage_service(elevated_arg: &str, direct: fn() -> Result<(), String>) -> Result<(), String> {
    if elevation::is_elevated() {
        direct()
    } else {
        elevation::run_elevated(elevated_arg)
    }
}
