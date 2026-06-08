// Hide the console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // The SCM launches the installed service as `AllTheThings.exe --service`.
    // That branch runs the headless index service (no Tauri window) and must be
    // taken before the GUI loop. A normal launch never sees this flag.
    if std::env::args().any(|arg| arg == "--service") {
        allthethings_lib::run_service();
        return;
    }
    allthethings_lib::run()
}
