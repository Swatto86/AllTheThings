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
use crate::infrastructure::{icons, startup};

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
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy().into_owned();
    if settings.run_at_startup {
        startup::register(&exe)?;
    } else {
        startup::unregister().ok();
    }
    *state.0.write() = settings.clone();
    settings::save(&settings).map_err(|e| e.to_string())?;
    Ok(())
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
// Read live from the SCM, never cached, so the Settings UI reconciles to reality
// (the way `get_settings` reconciles `run_at_startup` from the real logon task).
// Install/start/stop/uninstall require elevation; the GUI is still elevated in
// this phase, so these call straight through — surfacing an access-denied as a
// clear message if it ever runs unelevated.

/// The service's current SCM state, for the Settings status row.
#[tauri::command]
pub fn service_status() -> Result<SvcState, String> {
    scm::status()
}

/// Register the service (LocalSystem, auto-start, this exe + `--service`).
#[tauri::command]
pub fn install_service() -> Result<(), String> {
    scm::install()
}

/// Stop (if running) and remove the service registration.
#[tauri::command]
pub fn uninstall_service() -> Result<(), String> {
    scm::uninstall()
}

/// Start the installed service.
#[tauri::command]
pub fn start_service() -> Result<(), String> {
    scm::start()
}

/// Stop the running service.
#[tauri::command]
pub fn stop_service() -> Result<(), String> {
    scm::stop()
}
