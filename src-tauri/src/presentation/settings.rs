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
    /// Whether the elevated logon scheduled task is registered. Reconciled
    /// against the real task on read.
    pub run_at_startup: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            close_to_tray: true,
            run_at_startup: false,
        }
    }
}

/// Tauri-managed settings, shared with the window-close handler.
pub struct SettingsState(pub RwLock<Settings>);

/// Launch flags derived from the command line (not persisted).
pub struct StartFlags {
    pub start_hidden: bool,
}

fn settings_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("AllTheThings").join("settings.json"))
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
