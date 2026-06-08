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
    delete_path, export_results, file_icon, file_type, get_settings, hotkey_active, index_status,
    initial_search, install_service, is_elevated, mark_service_prompt_seen, open_path, rename_path,
    reveal_path, search, service_status, set_hotkey, set_settings, setup_service, shell_action,
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

/// Run a one-shot admin command (used by the elevated relaunch and the NSIS
/// installer): the `--svc-*` service-management actions and the `--task-*` logon
/// scheduled-task actions, which both need elevation. Returns `Some(exit_code)`
/// (0 success, 1 failure) for a recognized command, or `None` for an unrecognized
/// one so the caller can fall through to the normal GUI rather than exit silently.
pub fn run_admin_command(arg: &str) -> Option<i32> {
    use infrastructure::service::scm;
    use infrastructure::startup;
    let result = match arg {
        "--svc-install" => scm::install(),
        "--svc-uninstall" => scm::uninstall(),
        "--svc-start" => scm::start(),
        "--svc-stop" => scm::stop(),
        // Install + start in one elevated process, so adopting the service from
        // the migration banner costs a single UAC prompt, not two.
        "--svc-setup" => scm::install().and_then(|()| scm::start()),
        "--task-install" => std::env::current_exe()
            .map_err(|e| e.to_string())
            .and_then(|exe| startup::register(&exe.to_string_lossy())),
        "--task-uninstall" => startup::unregister(),
        _ => return None,
    };
    Some(i32::from(result.is_err()))
}

/// One-time nudge for auto-updated installs that still rely on the elevated
/// logon task: if the service isn't installed but the logon task is, suggest
/// adopting the service (so the GUI can run unelevated). Shown at most once; the
/// checks run off the UI thread so they never delay startup.
fn maybe_suggest_service(app: &AppHandle) {
    if settings::service_prompt_seen() {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        use infrastructure::service::scm::{self, SvcState};
        let has_task = infrastructure::startup::task_exists();
        let no_service = matches!(scm::status(), Ok(SvcState::NotInstalled));
        if has_task && no_service {
            std::thread::sleep(std::time::Duration::from_millis(3000));
            // The frontend marks the prompt seen only when it actually shows the
            // banner, so a lost or too-early emit doesn't permanently consume the
            // one-time nudge.
            let _ = app.emit("suggest-service", ());
        }
    });
}

/// Extract the folder argument of `--search-here <path>` from a command line.
fn parse_search_here(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--search-here" {
            // A drive-root *background* click substitutes `%V` = `C:\`, and the
            // trailing `\"` escapes the closing quote in Windows arg-parsing,
            // arriving as e.g. `C:"`. Strip trailing quotes/backslashes so it
            // still scopes to the drive root rather than a garbage path.
            return it
                .next()
                .map(|p| p.trim_end_matches(['"', '\\']).to_string())
                .filter(|p| !p.is_empty());
        }
    }
    None
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    let start_hidden_flag = args.iter().any(|arg| arg == "--minimized");
    let search_here = parse_search_here(&args);

    // Detects the backend: queries the service if one is running, else starts
    // in-process indexing.
    let state = AppState::new();
    let settings = settings::load();
    let initial_hotkey = settings.hotkey.clone();

    tauri::Builder::default()
        // single-instance must be registered first: a second launch focuses the
        // running window (and, for an Explorer "Search here", scopes it to that
        // folder) instead of opening a duplicate.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            show_main(app);
            if let Some(path) = parse_search_here(&argv) {
                let _ = app.emit("search-here", path);
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(state)
        .manage(SettingsState(RwLock::new(settings)))
        .manage(StartFlags {
            start_hidden: start_hidden_flag,
            search_here,
        })
        .setup(move |app| {
            build_tray(app.handle())?;
            // Register the saved global hotkey; a failure (e.g. another app owns
            // it) is non-fatal — the app just starts without it.
            if let Err(e) = presentation::hotkey::apply(app.handle(), &initial_hotkey) {
                eprintln!("[hotkey] {e}");
            }
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
            maybe_suggest_service(app.handle());
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
            stop_service,
            setup_service,
            mark_service_prompt_seen,
            is_elevated,
            set_hotkey,
            hotkey_active,
            initial_search
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
