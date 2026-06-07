fn main() {
    // Embed a Windows manifest that forces elevation (requireAdministrator) so
    // launching the app always prompts for UAC — raw NTFS volume access needs it.
    let windows = tauri_build::WindowsAttributes::new()
        .app_manifest(include_str!("AllTheThings.manifest"));
    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
        .expect("failed to run tauri-build");
}
