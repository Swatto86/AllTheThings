# Manual test — background service (Phases 2–4)

The dev/CI shell is **non-elevated**, so `CreateService`, start/stop, and the cross-privilege pipe can
only be exercised by hand under an elevated token. This is the script to run on a real machine after a
build. The GUI still ships **elevated** in this milestone (de-elevation is Phase 5), so every step here is
run As Administrator.

Service name: **`AllTheThingsSvc`**  ·  Pipe: `\\.\pipe\AllTheThings`  ·  Service binPath: `<exe> --service`

## 0. Build the binary to test

```powershell
# from the repo root, non-elevated is fine for the build itself
npm run tauri build           # release installer + exe, OR:
cargo build --manifest-path src-tauri/Cargo.toml   # debug exe at src-tauri/target/debug/allthethings.exe
```

Note the exe path you'll test (installed `...\AllTheThings\AllTheThings.exe`, or the `target` exe).

## 1. Register + start the service via the GUI (elevated)

1. Launch the GUI **As Administrator**. It indexes in-process (no service yet) — status bar reads
   `Ready · N items …` with **no** `· via service` suffix.
2. Open **Settings (⚙)** → the **Background index service** row should read **Not installed** with an
   **Install** button.
3. Click **Install** → the row reconciles to **Installed · stopped** (Uninstall + Start appear).
4. Click **Start** → it should settle on **Running** (it may flash *Starting…*).

## 2. Confirm the service is real (elevated PowerShell)

```powershell
sc query AllTheThingsSvc
# expect: STATE : 4  RUNNING
sc qc AllTheThingsSvc
# expect: BINARY_PATH_NAME ... AllTheThings.exe --service ; START_TYPE : 2 AUTO_START ;
#         SERVICE_START_NAME : LocalSystem
Get-CimInstance Win32_Service -Filter "Name='AllTheThingsSvc'" | Select Name,State,StartMode,StartName
```

Optional pipe sanity check (the DACL grants Authenticated Users, so this works even from a **non**-elevated
shell once the service is running):

```powershell
# should NOT throw; a successful open proves the pipe exists and AU may connect
$h = [System.IO.File]::Open('\\.\pipe\AllTheThings','Open','ReadWrite','None'); $h.Close()
```

## 3. Confirm the GUI uses the service

1. **Fully quit** the GUI (tray → Quit) and relaunch it **As Administrator**. The backend is chosen at
   launch, so it must be restarted to pick up the now-running service.
2. Status bar should now read `Ready · N items … · **via service**`.
3. Run a few searches — results, counts, and timing should match in-process behaviour.
4. In **Settings**, the service row should read **Running · in use**, and the hint should say search is
   served by the service.

## 4. Manage from Settings

- **Stop** → row settles on **Installed · stopped**; `sc query AllTheThingsSvc` shows `STOPPED`. The GUI
  (still pointed at the service for this session) should now show a **degraded** status
  (`Index error: background service unavailable …`) rather than silently re-indexing — that is by design
  (no mid-session fallback).
- **Start** again → back to **Running**.
- **Uninstall** → row returns to **Not installed**; `sc query AllTheThingsSvc` reports
  `(1060) … does not exist`. Relaunching the GUI then indexes in-process again (no `· via service`).

## 5. Negative / robustness checks (optional)

- Run `AllTheThings.exe --service` **from a console** (not via SCM): it should exit quietly (the SCM
  dispatcher connect fails) — **no** window, no hang.
- With the service **uninstalled**, launch the GUI: startup must not stall waiting for a pipe (fast
  fall-back to in-process) and search must work.
- While the service is **Running**, try **Install** again in Settings: it stays consistent (idempotent),
  no spurious error.

## What "pass" looks like

`sc query` shows `RUNNING` with binPath `… --service` under `LocalSystem`; the GUI shows `· via service`
after a restart; Stop/Start/Uninstall from Settings track `sc query` exactly; stopping the service
degrades (not crashes) the GUI; and `--service` from a console exits cleanly.
