//! The user-configurable global hotkey that summons (toggles) the main window
//! from anywhere. The accelerator is a Tauri shortcut string (e.g.
//! `"Ctrl+Alt+Space"`); an empty string disables it.

use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

/// (Re)register the global hotkey, clearing any previously registered one first.
/// An empty/blank `accel` just clears it (disabled). Returns an error if the
/// accelerator is invalid or the OS rejects it (e.g. another app owns it).
pub fn apply(app: &AppHandle, accel: &str) -> Result<(), String> {
    let shortcuts = app.global_shortcut();
    shortcuts.unregister_all().map_err(|e| e.to_string())?;

    let accel = accel.trim();
    if accel.is_empty() {
        return Ok(());
    }

    shortcuts
        .on_shortcut(accel, |app, _shortcut, event| {
            // Fire on press only; the plugin also reports key release.
            if event.state() == ShortcutState::Pressed {
                toggle(app);
            }
        })
        .map_err(|e| format!("could not register hotkey '{accel}': {e}"))
}

/// Whether `accel` is actually registered as a live global shortcut (the OS
/// accepted it). Lets the UI reconcile a stored-but-inactive hotkey — e.g. when
/// another app already owns the combo so startup registration silently failed.
pub fn is_active(app: &AppHandle, accel: &str) -> bool {
    let accel = accel.trim();
    if accel.is_empty() {
        return false;
    }
    match accel.parse::<Shortcut>() {
        Ok(shortcut) => app.global_shortcut().is_registered(shortcut),
        Err(_) => false,
    }
}

/// Toggle the main window: hide it when it is the focused foreground window,
/// otherwise bring it forward and focus it (a summon/dismiss gesture).
fn toggle(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let showing = window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false);
    if showing {
        let _ = window.hide();
    } else {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
