//! User settings, persisted to `%LOCALAPPDATA%\AllTheThings\settings.json`.

use std::io;
use std::path::PathBuf;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Closing the window hides it to the tray instead of quitting.
    pub close_to_tray: bool,
    /// Whether the logon scheduled task is registered. Reconciled against the
    /// real task on read.
    pub run_at_startup: bool,
    /// Whether the Explorer "Search here" context-menu entry is registered.
    /// Reconciled against the real registry key on read.
    pub explorer_menu: bool,
    /// Global hotkey accelerator that summons the window (e.g. `"Ctrl+Alt+Space"`);
    /// an empty string disables it. Owned by the `set_hotkey` command.
    pub hotkey: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            close_to_tray: true,
            run_at_startup: false,
            explorer_menu: false,
            hotkey: "Ctrl+Alt+Space".into(),
        }
    }
}

/// Tauri-managed settings, shared with the window-close handler.
pub struct SettingsState(pub RwLock<Settings>);

/// Launch flags derived from the command line (not persisted).
pub struct StartFlags {
    pub start_hidden: bool,
    /// A folder passed via `--search-here` (the Explorer context menu) for the
    /// first instance to scope its initial search to.
    pub search_here: Option<String>,
}

fn settings_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(
        PathBuf::from(base)
            .join("AllTheThings")
            .join("settings.json"),
    )
}

/// Marker that the one-time service-migration prompt has been shown. Kept as a
/// separate file rather than a [`Settings`] field, because the frontend
/// overwrites the whole settings object on save and would otherwise clear it.
fn migration_marker_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(
        PathBuf::from(base)
            .join("AllTheThings")
            .join(".service-prompted"),
    )
}

/// Whether the service-migration prompt has already been shown (or can't be
/// tracked, in which case we don't nag).
pub fn service_prompt_seen() -> bool {
    migration_marker_path().map(|p| p.exists()).unwrap_or(true)
}

/// Record that the service-migration prompt has been shown.
pub fn mark_service_prompt_seen() {
    if let Some(path) = migration_marker_path() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, b"1");
    }
}

pub fn load() -> Settings {
    settings_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(settings: &Settings) -> io::Result<()> {
    let path = settings_path().ok_or_else(|| io::Error::other("LOCALAPPDATA not set"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(settings).map_err(io::Error::other)?;
    std::fs::write(path, json)
}
