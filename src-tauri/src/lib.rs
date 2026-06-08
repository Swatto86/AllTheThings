//! AllTheThings — a voidtools Everything clone.
//!
//! Layered: `domain` (pure types) ← `application` (search index + contracts) ←
//! `infrastructure` (NTFS MFT/USN, cache, icons, startup) and `presentation`
//! (Tauri commands, settings, tray).

mod application;
mod domain;
mod infrastructure;
mod presentation;

#[cfg(test)]
mod engine_tests;

use parking_lot::RwLock;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};

use presentation::commands::{
    delete_path, export_results, file_icon, file_type, get_settings, index_status, install_service,
    open_path, rename_path, reveal_path, search, service_status, set_settings, shell_action,
    start_hidden, start_service, stop_service, uninstall_service, uses_service,
};
use presentation::settings::{self, SettingsState, StartFlags};
use presentation::state::AppState;

/// Entry point when the SCM launches this exe with `--service`: run the headless
/// index service. Blocks until the service stops; never builds a window. A
/// failure (e.g. launched outside the SCM) just returns and the process exits.
pub fn run_service() {
    let _ = infrastructure::service::host::run();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let start_hidden_flag = std::env::args().any(|arg| arg == "--minimized");

    // Detects the backend: queries the service if one is running, else starts
    // in-process indexing.
    let state = AppState::new();
    let settings = settings::load();

    tauri::Builder::default()
        // single-instance must be registered first: a second launch focuses
        // the running window instead of opening a duplicate.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .manage(SettingsState(RwLock::new(settings)))
        .manage(StartFlags {
            start_hidden: start_hidden_flag,
        })
        .setup(move |app| {
            build_tray(app.handle())?;
            // Reveal the window after a delay unless we launched into the tray.
            // (The frontend normally shows it sooner, once painted.)
            if !start_hidden_flag {
                if let Some(window) = app.get_webview_window("main") {
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(2500));
                        let _ = window.show();
                    });
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let close_to_tray = window
                    .app_handle()
                    .state::<SettingsState>()
                    .0
                    .read()
                    .close_to_tray;
                if close_to_tray {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            search,
            index_status,
            uses_service,
            open_path,
            reveal_path,
            rename_path,
            delete_path,
            shell_action,
            export_results,
            file_icon,
            file_type,
            get_settings,
            set_settings,
            start_hidden,
            service_status,
            install_service,
            uninstall_service,
            start_service,
            stop_service
        ])
        .run(tauri::generate_context!())
        .expect("error while running AllTheThings");
}

/// Build the system-tray icon with a Show / Settings / Quit menu.
fn build_tray(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let show = MenuItem::with_id(app, "show", "Show AllTheThings", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &settings, &separator, &quit])?;

    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or("missing default window icon")?;

    TrayIconBuilder::with_id("main")
        .icon(icon)
        .tooltip("AllTheThings")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main(app),
            "settings" => {
                show_main(app);
                let _ = app.emit("open-settings", ());
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Bring the main window to the foreground.
fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
