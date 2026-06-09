//! The Explorer "Search AllTheThings here" context-menu entry, as a per-user
//! (HKCU) static shell verb — no admin needed, fitting the unelevated GUI.
//! Right-clicking a folder, or the background of an open folder, launches the
//! app scoped to that path via `--search-here`. On Windows 11 it appears under
//! "Show more options" (the legacy menu); a main-menu entry would need a
//! packaged `IExplorerCommand`, which is out of scope.

use std::iter::once;
use std::ptr;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};

const VERB_LABEL: &str = "Search AllTheThings here";
/// Right-click ON a folder.
const DIRECTORY_KEY: &str = r"Software\Classes\Directory\shell\AllTheThings";
/// Right-click the background of an open folder.
const BACKGROUND_KEY: &str = r"Software\Classes\Directory\Background\shell\AllTheThings";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(once(0)).collect()
}

/// Whether the context-menu entry is fully registered — both the folder and the
/// folder-background verbs. Requiring both means a partial register (one key
/// written, the other failed) honestly reconciles as "off" so the next toggle
/// rewrites both.
pub fn is_registered() -> bool {
    key_exists(DIRECTORY_KEY) && key_exists(BACKGROUND_KEY)
}

fn key_exists(subkey: &str) -> bool {
    let sub = wide(subkey);
    let mut key: HKEY = ptr::null_mut();
    // SAFETY: null-terminated subkey; `key` is a valid out-pointer.
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_READ, &mut key) };
    if rc == ERROR_SUCCESS {
        // SAFETY: `key` was opened by the call above.
        unsafe { RegCloseKey(key) };
        true
    } else {
        false
    }
}

/// Register (or refresh) the entry, pointing at the current executable. The
/// command passes the clicked path (`%1`) or the folder background (`%V`).
pub fn register() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy();
    write_verb(
        DIRECTORY_KEY,
        &exe,
        &format!("\"{exe}\" --search-here \"%1\""),
    )?;
    write_verb(
        BACKGROUND_KEY,
        &exe,
        &format!("\"{exe}\" --search-here \"%V\""),
    )?;
    Ok(())
}

/// Remove the entry. Succeeds even if it is already absent. Both keys are always
/// attempted (not short-circuited) so a transient failure on the first can't
/// strand the second — a later retry then cleans whichever key remains.
pub fn unregister() -> Result<(), String> {
    let dir = delete_tree(DIRECTORY_KEY);
    let bg = delete_tree(BACKGROUND_KEY);
    dir.and(bg)
}

/// Write one verb: the label + icon on the base key, and the command on its
/// `command` subkey. On any failure after the base key is created, the whole
/// subtree is rolled back — `is_registered()` only checks the base key, so a
/// half-written verb (base present but command missing) would otherwise report
/// as fully registered while the menu entry is inert.
fn write_verb(base: &str, icon: &str, command: &str) -> Result<(), String> {
    let result = (|| {
        let key = create_key(base)?;
        let label = set_string(key, None, VERB_LABEL);
        let icon = set_string(key, Some("Icon"), icon);
        // SAFETY: `key` from create_key.
        unsafe { RegCloseKey(key) };
        label?;
        icon?;

        let cmd_key = create_key(&format!(r"{base}\command"))?;
        let cmd = set_string(cmd_key, None, command);
        // SAFETY: `cmd_key` from create_key.
        unsafe { RegCloseKey(cmd_key) };
        cmd
    })();
    if result.is_err() {
        let _ = delete_tree(base);
    }
    result
}

fn create_key(subkey: &str) -> Result<HKEY, String> {
    let sub = wide(subkey);
    let mut key: HKEY = ptr::null_mut();
    // SAFETY: null-terminated subkey; `key` a valid out-pointer; null class /
    // security; standard non-volatile create with write access.
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            0,
            ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            ptr::null(),
            &mut key,
            ptr::null_mut(),
        )
    };
    if rc == ERROR_SUCCESS {
        Ok(key)
    } else {
        Err(format!("registry create failed for {subkey} (error {rc})"))
    }
}

fn set_string(key: HKEY, name: Option<&str>, data: &str) -> Result<(), String> {
    let name_w = name.map(wide);
    let name_ptr = name_w.as_ref().map_or(ptr::null(), |w| w.as_ptr());
    let data_w = wide(data);
    let bytes = data_w.len() * 2; // wide units incl. the null terminator
                                  // SAFETY: `data_w` is a valid wide buffer of `bytes` bytes; name optional.
    let rc = unsafe {
        RegSetValueExW(
            key,
            name_ptr,
            0,
            REG_SZ,
            data_w.as_ptr() as *const u8,
            bytes as u32,
        )
    };
    if rc == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("registry set value failed (error {rc})"))
    }
}

fn delete_tree(subkey: &str) -> Result<(), String> {
    let sub = wide(subkey);
    // SAFETY: null-terminated subkey; deletes the key and its descendants.
    let rc = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, sub.as_ptr()) };
    if rc == ERROR_SUCCESS || rc == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(format!("registry delete failed for {subkey} (error {rc})"))
    }
}
