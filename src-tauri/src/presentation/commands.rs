//! Tauri command handlers — the only place the UI touches the backend.

use std::os::windows::process::CommandExt;
use std::process::Command;

use tauri::{Emitter, State};
use tauri_plugin_opener::OpenerExt;

use crate::application::{IndexStatus, SearchOptions, SearchResult};
use crate::infrastructure::fileops::{self, ShellVerb};
use crate::infrastructure::{icons, startup};

use super::settings::{self, Settings, SettingsState, StartFlags};
use super::state::AppState;

/// Search the catalog with the given options, returning ranked hits.
#[tauri::command]
pub fn search(state: State<'_, AppState>, options: SearchOptions) -> SearchResult {
    state.catalog.read().search(&options)
}

/// Report indexing progress / readiness for the status bar.
#[tauri::command]
pub fn index_status(state: State<'_, AppState>) -> IndexStatus {
    state.status()
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
