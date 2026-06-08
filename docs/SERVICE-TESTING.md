# Manual test — background service & de-elevation (Phases 2–5)

The dev/CI shell is **non-elevated**, so `CreateService`, start/stop, the cross-privilege pipe, and the
UAC relaunches can only be exercised by hand. This is the script to run on a real machine after a build.
As of Phase 5 the GUI ships **`asInvoker`** (unelevated); indexing is done by the LocalSystem service,
and the few admin operations relaunch elevated on demand.

Service: **`AllTheThingsSvc`** (LocalSystem, auto-start) · Pipe: `\\.\pipe\AllTheThings` (query-only, AU) ·
Service binPath: `<exe> --service` · Logon task: `AllTheThings` (`/rl highest`, fallback + auto-launch)

## 1. Fresh install (the main path)

1. Run `AllTheThings_0.7.0_x64-setup.exe` (it self-elevates). The installer runs `--svc-install` +
   `--svc-start` and registers the logon task.
2. Confirm the service:
   ```powershell
   sc query AllTheThingsSvc      # STATE: 4 RUNNING
   sc qc AllTheThingsSvc         # BINARY_PATH_NAME ... \AllTheThings.exe --service ; AUTO_START ; LocalSystem
   ```
3. Launch the app **from the Start Menu** (a normal, *unelevated* launch). In Task Manager → Details, add
   the **Elevated** column: `AllTheThings.exe` should read **No**.
4. Status bar reads `Ready · N items … · via service`. Searches work — **with no UAC prompt at launch**.

## 2. Manage the service from the unelevated GUI (UAC on demand)

Open **Settings (⚙)** → **Background index service** row (hint should mention "Managing it prompts for admin").

- **Stop** → a **UAC prompt** appears → accept → row shows `Installed · stopped`; `sc query` → `STOPPED`.
  The running GUI (still bound to the service this session) now shows a **degraded** status
  (`Index error: background service unavailable …`) — by design (no mid-session re-index).
- **Start** → UAC → `Running`.
- **Uninstall** → UAC → `Not installed`; `sc query AllTheThingsSvc` → `(1060) does not exist`.
- **Install** → UAC → `Installed · stopped` → **Start** → `Running`.
- Decline a UAC prompt once → the action reports "elevation was declined" and the row reconciles to the
  unchanged real state (no stale UI).

## 3. Unelevated with no service (fallbacks)

1. Uninstall the service (step 2) and fully quit the app (tray → Quit).
2. Relaunch **from the Start Menu** (unelevated). Status shows: *couldn't read the NTFS volumes (…) —
   install the background service in Settings, or launch AllTheThings as administrator.* (No crash.)
3. Settings → **Install** → UAC → install, then **Start** → restart the app → `· via service` again.
4. Elevated fallback: with the service uninstalled, either let the **logon task** launch it at next
   sign-in, or right-click → **Run as administrator**. An elevated GUI indexes **in-process** and works
   without the service. (When the service *is* present, an elevated GUI still uses it — no double-index.)

## 4. "Start with Windows" toggle (now needs UAC)

Settings → toggle **Start with Windows** off, then on. Each change should trigger a **UAC prompt**
(the task uses `/rl highest`). Verify: `schtasks /query /tn AllTheThings` exists/absent accordingly.
Toggling **Close to tray** must **not** prompt (no task change).

## 5. Migration nudge (auto-updated installs)

Simulate an install that has the task but not the service:
```powershell
schtasks /create /tn "AllTheThings" /tr "\"C:\Path\AllTheThings.exe\" --minimized" /sc onlogon /rl highest /f
# ensure the service is NOT installed; delete the marker so the prompt can show:
Remove-Item "$env:LOCALAPPDATA\AllTheThings\.service-prompted" -ErrorAction SilentlyContinue
```
Launch the app unelevated. After ~3 s a banner appears: *AllTheThings can now index in the background
without admin. Install the service?* → **Install service** → UAC → installs + starts. Relaunch the app:
the banner must **not** reappear (one-time; marker file written).

## 6. Upgrade in place

Install v0.6.0, then run the v0.7.0 installer over it (or let auto-update do it). Confirm: **no stray GUI
window** pops up during install/uninstall, the service ends up **running**, and the upgraded app launches
**unelevated** with `· via service`.

## 7. CLI one-shots (no window, exit code only)

From an **elevated** PowerShell:
```powershell
& 'C:\...\AllTheThings.exe' --svc-install ;  $LASTEXITCODE   # 0
& 'C:\...\AllTheThings.exe' --svc-start   ;  $LASTEXITCODE   # 0
& 'C:\...\AllTheThings.exe' --svc-stop    ;  $LASTEXITCODE
& 'C:\...\AllTheThings.exe' --svc-uninstall
& 'C:\...\AllTheThings.exe' --task-install ; & 'C:\...\AllTheThings.exe' --task-uninstall
```
Each must run **without opening a window** and exit 0 on success. Running `--service` from a console (not
the SCM) must also exit quietly with no window.

## What "pass" looks like

Fresh install → `sc query` RUNNING, app launches **unelevated** (Elevated = No) and shows `· via service`,
**no UAC at launch**; Settings management each triggers one UAC prompt and tracks `sc query`; declining UAC
degrades gracefully; with no service the GUI guides you to install it or run elevated, and the elevated
logon task still indexes in-process; the migration banner appears once; and an upgrade leaves the service
running with no stray window.
