// Hide the console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // The SCM launches the installed service as `AllTheThings.exe --service`.
    // That branch runs the headless index service (no Tauri window) and must be
    // taken before the GUI loop. A normal launch never sees this flag.
    if args.iter().any(|arg| arg == "--service") {
        allthethings_lib::run_service();
        return;
    }
    // One-shot admin commands used by the elevated relaunch and the NSIS
    // installer: `--svc-*` (service) and `--task-*` (logon task). A recognized
    // command runs, exits with its status, and never builds a window; an
    // unrecognized one falls through to the normal GUI rather than exiting
    // silently with no window.
    if let Some(cmd) = args
        .iter()
        .find(|arg| arg.starts_with("--svc-") || arg.starts_with("--task-"))
    {
        if let Some(code) = allthethings_lib::run_admin_command(cmd) {
            std::process::exit(code);
        }
    }
    allthethings_lib::run()
}
