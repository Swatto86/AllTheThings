//! Tauri command handlers — the only place the UI touches the backend.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::os::windows::process::CommandExt;
use std::process::Command;

use tauri::{Emitter, State};
use tauri_plugin_opener::OpenerExt;

use std::sync::atomic::Ordering;

use crate::application::export::{self, ExportFormat};
use crate::application::search::{content_scope_ok, extract_content};

/// Shown when `content:` is combined with a top-level OR, which can't be scoped.
const CONTENT_OR_ERR: &str =
    "content: applies to every result and can't be combined with a top-level OR (|). \
     Put the alternation in parentheses, e.g. content:foo (a | b).";
use crate::application::{IndexStatus, SearchOptions, SearchResult};
use crate::infrastructure::fileops::{self, ShellVerb};
use crate::infrastructure::service::scm::{self, SvcState};
use crate::infrastructure::{content, elevation, icons, shellmenu, startup};

use super::hotkey;
use super::settings::{self, Settings, SettingsState, StartFlags};
use super::state::AppState;

/// Search via the active backend (the service, or the in-process index).
#[tauri::command]
pub fn search(state: State<'_, AppState>, options: SearchOptions) -> SearchResult {
    // Switching to a plain search means any in-flight content scan's result would
    // be discarded — bump the generation so it abandons early instead of reading
    // tens of thousands of file bodies to completion in the background.
    state.next_content_gen();
    state.search(&options)
}

/// Report indexing progress / readiness for the status bar.
#[tauri::command]
pub fn index_status(state: State<'_, AppState>) -> IndexStatus {
    state.status()
}

/// Candidate cap for content search: the index narrows to this many files, whose
/// bodies are then grepped. Bounds the work — the user narrows by filename to fit
/// (e.g. `*.log content:"timeout"`); a bare `content:` scans the first N files.
const CONTENT_CANDIDATE_CAP: usize = 50_000;

/// Search inside files: `content:"term"` keeps the files (matched by the rest of
/// the query) whose body contains every content term. Two phases — the index
/// narrows candidates (fast, possibly via the service), then their bodies are
/// read **in this process's user token** (the query-only service never reads
/// file contents). Async + a cancellation generation so typing supersedes an
/// in-flight scan instead of queueing behind it.
#[tauri::command]
pub async fn search_content(
    state: State<'_, AppState>,
    options: SearchOptions,
) -> Result<SearchResult, String> {
    let (clean_query, terms) = extract_content(&options.query);
    let handle = state.search_handle();

    // No content term yet (e.g. mid-typing a bare `content:`): fall back to a
    // plain index search on the CLEANED query — searching the raw query would
    // look for a literal "content:" and wrongly return nothing. Run off the
    // async worker too, so a blocking pipe round-trip never occupies it.
    if terms.is_empty() {
        // Dropped the content term — abandon any prior in-flight content scan too.
        state.next_content_gen();
        let mut opts = options;
        opts.query = clean_query;
        return tauri::async_runtime::spawn_blocking(move || handle.search(&opts))
            .await
            .map_err(|e| e.to_string());
    }

    if !content_scope_ok(&options.query) {
        return Err(CONTENT_OR_ERR.into());
    }

    let generation = state.next_content_gen();
    let gen_flag = state.content_gen_handle();
    let mut index_opts = options.clone();
    index_opts.query = clean_query;
    index_opts.limit = CONTENT_CANDIDATE_CAP;
    let display_limit = options.limit;
    let match_case = options.match_case;

    // Both phases run off the async (tokio) worker via spawn_blocking: the index
    // round-trip (pipe I/O or a large local scan) AND the file-body reads (always
    // in this process's user token — the service never reads file contents). The
    // grep abandons candidates as soon as a newer content search supersedes this.
    tauri::async_runtime::spawn_blocking(move || {
        let started = std::time::Instant::now();
        let candidates = handle.search(&index_opts);
        if candidates.error.is_some() {
            return candidates;
        }
        // The filename narrowing matched more files than we'll grep, so the scan
        // is partial — surfaced so the UI can say so rather than imply complete.
        let capped = candidates.total > CONTENT_CANDIDATE_CAP;
        let cancelled = || gen_flag.load(Ordering::SeqCst) != generation;
        let mut hits = content::filter_by_content(candidates.hits, &terms, match_case, &cancelled);
        // Report the true match count, not the post-truncate length, so the UI's
        // "N in files" reflects reality (mirrors the plain index path) even though
        // only `display_limit` rows are returned for the virtual list.
        let matched = hits.len();
        hits.truncate(display_limit);
        SearchResult {
            total: matched,
            took_ms: started.elapsed().as_millis(),
            hits,
            error: None,
            capped,
        }
    })
    .await
    .map_err(|e| e.to_string())
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
    /// A content export whose candidate pool was truncated (incomplete scan).
    pub capped: bool,
}

/// Export the current results to `path` as `format` (`csv`/`txt`/`efu`), re-running
/// the search unbounded (up to [`EXPORT_CAP`]).
#[tauri::command]
pub async fn export_results(
    state: State<'_, AppState>,
    options: SearchOptions,
    format: String,
    path: String,
) -> Result<ExportSummary, String> {
    let fmt = ExportFormat::parse(&format).ok_or_else(|| format!("unknown format '{format}'"))?;
    let handle = state.search_handle();
    let (clean_query, terms) = extract_content(&options.query);
    let content_search = !terms.is_empty();
    if content_search && !content_scope_ok(&options.query) {
        return Err(CONTENT_OR_ERR.into());
    }

    let mut opts = options;
    opts.query = clean_query;
    // A content export is bounded by the candidate cap (each file is read), not
    // the much larger row cap for a pure index export.
    opts.limit = if content_search {
        CONTENT_CANDIDATE_CAP
    } else {
        EXPORT_CAP
    };

    // Run the whole export — index search, content scan, and disk write — off the
    // async worker via spawn_blocking, so a large export never freezes the UI
    // thread (mirrors search_content; file bodies are read in our user token).
    tauri::async_runtime::spawn_blocking(move || {
        let mut result = handle.search(&opts);
        if let Some(e) = result.error {
            return Err(e);
        }
        // For a content export the candidate pool (and thus the scan) is capped;
        // the pre-filter total is the only signal that files were dropped, so
        // capture it before overwriting `total` with the post-filter match count.
        let content_capped = content_search && result.total > CONTENT_CANDIDATE_CAP;
        if content_search {
            let never_cancel = || false;
            result.hits =
                content::filter_by_content(result.hits, &terms, opts.match_case, &never_cancel);
            result.total = result.hits.len();
        }

        let file = File::create(&path).map_err(|e| e.to_string())?;
        let mut writer = BufWriter::new(file);
        export::write_export(&result.hits, fmt, &mut writer).map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
        Ok(ExportSummary {
            written: result.hits.len(),
            total: result.total,
            capped: content_capped,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Open a file or folder with its default handler.
#[tauri::command]
pub fn open_path(app: tauri::AppHandle, path: String) -> Result<(), String> {
    // Surface a stale result clearly instead of relying on the handler's error.
    if !std::path::Path::new(&path).exists() {
        return Err("The item no longer exists at that location".into());
    }
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())
}

/// The shell icon for a file extension (or folder), as a base64 PNG. Async +
/// spawn_blocking so a slow third-party shell icon handler can't stall the UI
/// thread (SHGetFileInfoW may instantiate a COM icon handler on first touch).
#[tauri::command]
pub async fn file_icon(ext: Option<String>, is_dir: bool) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || icons::icon_base64(ext.as_deref(), is_dir))
        .await
        .ok()
        .flatten()
}

/// The registry's friendly type name for a file extension (or folder). Async for
/// the same reason as [`file_icon`].
#[tauri::command]
pub async fn file_type(ext: Option<String>, is_dir: bool) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || icons::type_name(ext.as_deref(), is_dir))
        .await
        .ok()
        .flatten()
}

/// Current user settings, with the toggles backed by external state reconciled
/// against reality: `run_at_startup` from the real logon task, `explorer_menu`
/// from the real registry key.
#[tauri::command]
pub fn get_settings(state: State<'_, SettingsState>) -> Settings {
    let mut settings = state.0.read().clone();
    settings.run_at_startup = startup::task_exists();
    settings.explorer_menu = shellmenu::is_registered();
    settings
}

/// Persist settings and apply side effects (logon task + Explorer menu). The
/// global hotkey is owned by `set_hotkey` and preserved here untouched.
#[tauri::command]
pub async fn set_settings(
    state: State<'_, SettingsState>,
    mut settings: Settings,
) -> Result<(), String> {
    // Apply the two external side effects, but capture the FIRST error rather than
    // returning early: we must still reconcile and persist below, so a failure in
    // one step never silently discards the user's other changes (e.g.
    // close_to_tray) or leaves memory/disk disagreeing with the real system.
    let mut side_effect_err: Option<String> = None;

    // Creating/deleting the logon task needs admin (tasks under the root folder
    // require it). Only touch it when the toggle actually changed (reconciled
    // from the real task), and relaunch elevated when the GUI isn't — mirroring
    // the service commands. The task creation and the elevated `runas` relaunch
    // (UAC prompt + waiting on the child) run on a blocking worker so they don't
    // freeze the window.
    if settings.run_at_startup != startup::task_exists() {
        let enable = settings.run_at_startup;
        let r = tauri::async_runtime::spawn_blocking(move || {
            if elevation::is_elevated() {
                apply_startup_task(enable)
            } else {
                let arg = if enable {
                    "--task-install"
                } else {
                    "--task-uninstall"
                };
                elevation::run_elevated(arg)
            }
        })
        .await
        .map_err(|e| e.to_string())?;
        if let Err(e) = r {
            side_effect_err = Some(e);
        }
    }

    // The Explorer "Search here" entry lives under HKCU — no elevation needed.
    if settings.explorer_menu != shellmenu::is_registered() {
        let r = if settings.explorer_menu {
            shellmenu::register()
        } else {
            shellmenu::unregister()
        };
        if let Err(e) = r {
            side_effect_err.get_or_insert(e);
        }
    }

    // Reconcile the externally-backed toggles from reality so a failed/half-applied
    // side effect never leaves a stale stored value, then persist — even on a
    // side-effect error — so non-external fields (close_to_tray) are not lost.
    settings.run_at_startup = startup::task_exists();
    settings.explorer_menu = shellmenu::is_registered();
    {
        // One write guard so preserving `hotkey` (owned by set_hotkey, which also
        // (re)registers the shortcut) is atomic w.r.t. a concurrent set_hotkey —
        // a read-then-separate-write could lose that update.
        let mut guard = state.0.write();
        settings.hotkey = guard.hotkey.clone();
        *guard = settings.clone();
    }
    let save_res = settings::save(&settings).map_err(|e| e.to_string());

    // Surface the side-effect error first: it occurred first and an
    // elevation/registry failure is usually more actionable than a save error.
    // Either way the reconciled state was already persisted above.
    match side_effect_err {
        Some(e) => Err(e),
        None => save_res,
    }
}

/// Set (and live-register) the global summon hotkey. An empty accelerator
/// disables it. On failure the previously-registered hotkey is restored, so a
/// rejected change never leaves the app with no working hotkey.
#[tauri::command]
pub fn set_hotkey(
    app: tauri::AppHandle,
    state: State<'_, SettingsState>,
    hotkey: String,
) -> Result<(), String> {
    if let Err(e) = hotkey::apply(&app, &hotkey) {
        let _ = hotkey::apply(&app, &state.0.read().hotkey);
        return Err(e);
    }
    let mut settings = state.0.write();
    settings.hotkey = hotkey;
    settings::save(&settings).map_err(|e| e.to_string())
}

/// Whether the stored global hotkey is currently registered with the OS, so the
/// UI can flag a hotkey that silently failed to bind (e.g. owned by another app).
#[tauri::command]
pub fn hotkey_active(app: tauri::AppHandle, state: State<'_, SettingsState>) -> bool {
    hotkey::is_active(&app, &state.0.read().hotkey)
}

/// Suspend the global hotkey while the user is (re)binding it in Settings, so
/// pressing the currently-bound combo is delivered to the capture box instead of
/// triggering the shortcut (which would hide the window mid-rebind).
#[tauri::command]
pub fn suspend_hotkey(app: tauri::AppHandle) -> Result<(), String> {
    hotkey::apply(&app, "")
}

/// Re-register the stored global hotkey after a rebind was cancelled (the capture
/// box lost focus without committing a new combo).
#[tauri::command]
pub fn resume_hotkey(app: tauri::AppHandle, state: State<'_, SettingsState>) -> Result<(), String> {
    let stored = state.0.read().hotkey.clone();
    hotkey::apply(&app, &stored)
}

/// The folder passed via `--search-here` at launch (the Explorer context menu),
/// for the frontend to scope its first search to. `None` for a normal launch.
#[tauri::command]
pub fn initial_search(flags: State<'_, StartFlags>) -> Option<String> {
    flags.search_here.clone()
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
    // The path is interpolated into explorer's quoted /select argument, so reject
    // a `"` or control char that could break out of the quoting (defence in depth
    // — real NTFS names can't contain `"`).
    if path.contains('"') || path.chars().any(|c| c.is_control()) {
        return Err("invalid path".into());
    }
    // Report a stale result (moved/deleted out of band) rather than launching a
    // default Explorer window and returning Ok as if it had revealed the item.
    if !std::path::Path::new(&path).exists() {
        return Err("The item no longer exists at that location".into());
    }
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
pub async fn install_service() -> Result<(), String> {
    manage_service("--svc-install", scm::install).await
}

/// Stop (if running) and remove the service registration.
#[tauri::command]
pub async fn uninstall_service() -> Result<(), String> {
    manage_service("--svc-uninstall", scm::uninstall).await
}

/// Start the installed service.
#[tauri::command]
pub async fn start_service() -> Result<(), String> {
    manage_service("--svc-start", scm::start).await
}

/// Stop the running service.
#[tauri::command]
pub async fn stop_service() -> Result<(), String> {
    manage_service("--svc-stop", scm::stop).await
}

/// Install **and** start the service in one elevated step (one UAC prompt),
/// used by the migration banner so adopting the service isn't two prompts.
#[tauri::command]
pub async fn setup_service() -> Result<(), String> {
    manage_service("--svc-setup", setup_service_direct).await
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
///
/// Runs on a blocking worker, not the UI thread: the `runas` UAC prompt and the
/// `WaitForSingleObject` on the elevated child (and the SCM calls in the
/// already-elevated path) would otherwise freeze the window until they finish.
async fn manage_service(
    elevated_arg: &'static str,
    direct: fn() -> Result<(), String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        if elevation::is_elevated() {
            direct()
        } else {
            elevation::run_elevated(elevated_arg)
        }
    })
    .await
    .map_err(|e| e.to_string())?
}
