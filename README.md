# AllTheThings

[![CI](https://github.com/Swatto86/AllTheThings/actions/workflows/ci.yml/badge.svg)](https://github.com/Swatto86/AllTheThings/actions/workflows/ci.yml)
[![Release](https://github.com/Swatto86/AllTheThings/actions/workflows/release.yml/badge.svg)](https://github.com/Swatto86/AllTheThings/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

A [voidtools Everything](https://www.voidtools.com/) clone — instant filename search for Windows, built in **Rust** with a **Tauri** UI.

Like Everything, it reads the **NTFS Master File Table** directly to index an entire volume in seconds, then tails the **USN change journal** to stay live as files are created, renamed, and deleted.

Measured on the developer's machine: **1.13M files indexed in ~3.4 s** (cold), substring search in **~15 ms**, `ext:` filter over the whole index in **~20 ms**, empty (browse-all) view in **0 ms**. After the first run the index is cached to disk and **warm starts load instantly**, replaying only the USN tail.

## Features

- **All fixed NTFS volumes** indexed and merged into one result set.
- **Search-as-you-type** with live USN updates.
- **Hardlink-aware**: a file appears at every path it is linked from (e.g. `System32\ntoskrnl.exe` and its WinSxS hardlinks).
- **Persistent index cache**: the index is saved to `%LOCALAPPDATA%\AllTheThings\cache`; subsequent launches load it instantly and replay only USN changes since (full rescan only if the journal was recreated or its tail purged).
- **No white flash on launch**: the window is shown only after the dark UI has painted.
- **Query syntax**: AND (space), **OR** (`|`), **NOT** (`!`), `"quoted phrases"`, `*`/`?` wildcards, regex, and `ext:`/`path:`/`file:`/`folder:`/`size:`/`dm:`/`dc:`/`da:`/`attrib:` functions (e.g. `ext:dll | ext:exe`, `report !draft size:>1mb`, `dm:thisweek`, `dc:2024-01-01..2024-06-30`, `attrib:h`). The date functions filter by **m**odified / **c**reated / **a**ccessed time and accept keywords (`today`, `yesterday`, `thisweek`/`thismonth`/`thisyear`, …), `YYYY[-MM[-DD]]` dates, `>`/`>=`/`<`/`<=` comparisons, and `A..B` ranges.
- **Matched-text highlighting** in results, and a **recent-searches** history dropdown.
- **Export results** — toolbar **Export** writes the current (full, filtered) result set to **CSV**, plain **text**, or an Everything **`.efu`** file list; the format follows the extension you choose in the Save dialog.
- **Toggles** (toolbar): match case, whole word, regular expression, match full path.
- **Size filter** dropdown: Empty / Tiny / Small / Medium / Large / Huge / Gigantic presets (inserts the matching `size:` range).
- **Shell file-type icons** — the real Windows icon per extension/folder, fetched on demand and cached.
- **Sortable, resizable, reorderable columns** — click a header to sort (again to reverse; Name/Path/Size/Date/Ext/Attributes), drag the edge to resize, drag the header to reorder. **Right-click a header** to pick columns: Name, Path, Size, Date modified/created/accessed, Type (registry file-type name), Ext, and Attributes; the layout is remembered. A toolbar **📁 toggle** groups folders first in any sort.
- **Right-click**: open, open containing folder (Explorer with the item selected), open with, run as administrator, copy full path / name, **rename** (inline, or F2), **delete** to the Recycle Bin (confirmed; or Del), and **Properties**.
- **Single-instance**: launching again focuses the running window.
- **System tray**: minimise/close to tray; Show / Settings / Quit menu; left-click to restore.
- **Auto-updates**: on launch it checks GitHub Releases for a newer **signed** build; an in-app banner offers one-click *Install & restart*. Also available from Settings → *Check now*.
- **Runs without admin** — indexing is delegated to a **background service** (`AllTheThingsSvc`, LocalSystem) that the installer registers and starts; the GUI runs `asInvoker` (unelevated) and queries it over a **query-only**, same-machine named pipe (the status bar shows *· via service*). All file actions (open/rename/delete/…) run in **your own** user context, never the service's. Manage the service (install / start / stop / uninstall) from **Settings** — a UAC prompt appears only when the GUI isn't already elevated.
- **Run at startup** (Settings): registers a logon scheduled task that auto-launches the app into the tray at sign-in. It runs elevated (`/rl highest`) so it can also index **in-process as a fallback** when the service isn't available.
- **Settings**: start-with-Windows, optional background-service management, and close-to-tray, persisted to `%LOCALAPPDATA%\AllTheThings\settings.json`.
- **Keyboard**: type to filter, ↑/↓ to move, Enter to open.

See [**docs/PARITY.md**](docs/PARITY.md) for a full feature-by-feature comparison with voidtools Everything and the roadmap.

## Status

Working build with daily-driver parity. The USN journal carries no size or full timestamps, so the live watcher re-reads each changed file's MFT record to fill in size, creation/modified/access times, and attributes — files created or renamed after the initial scan show complete metadata immediately.

## Download & install

Grab the latest installer from the [**Releases**](https://github.com/Swatto86/AllTheThings/releases) page (`AllTheThings_<version>_x64-setup.exe`) and run it — it installs and starts the background index **service**, registers an elevated logon task (auto-launch into the tray + fallback indexer), and the app itself then runs **without admin**. Uninstalling removes both.

Releases are built automatically by the [release workflow](.github/workflows/release.yml): **publish a GitHub release** for a `vX.Y.Z` tag (UI or `gh release create vX.Y.Z --generate-notes`) and the installer is built and attached to it. See [Releasing](#releasing) below.

## Requirements

- Windows with at least one **NTFS** volume
- **No admin to run** once the background service is installed (the installer does this). The GUI ships `asInvoker`; admin is requested (via UAC) only to install or manage the service — or, with no service present, the elevated logon task indexes in-process as a fallback. Reading the raw volume is itself privileged, which is exactly why the LocalSystem service does it on the GUI's behalf.
- Rust (pinned to 1.95.0 via `rust-toolchain.toml`) and Node 20+

## Verify the engine (no GUI)

The MFT/USN engine has a live integration test, ignored by default. Run it from an **elevated** terminal:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml -- --ignored --nocapture --test-threads=1
```

It indexes every NTFS volume and checks hardlink paths, wildcards, the `ext:`/`folder:`/`size:`/`attrib:` operators, the `dm:` date filter, and the cache round-trip.

## Run

```powershell
npm install
npm run tauri dev      # launch the app
```

In a dev build there's no installer to set up the service, so the unelevated GUI has nothing to read the volumes for it: either install the service once (`AllTheThings.exe --svc-install` from an elevated shell, or the Settings button), or run the terminal **As Administrator** so it indexes in-process. Without either, the status bar shows an access-denied error and results stay empty.

## Build an installer

```powershell
npm run tauri build    # produces an NSIS installer under src-tauri/target/release/bundle
```

## Releasing

Versions live in three files (`package.json`, `src-tauri/Cargo.toml`,
`src-tauri/tauri.conf.json`); the helper keeps them in lockstep:

```powershell
npm run bump -- 0.2.0          # set the version everywhere + sync Cargo.lock
git commit -am "Release v0.2.0"
git push
gh release create v0.2.0 --generate-notes   # creates the tag + release
```

Publishing the release triggers the [release workflow](.github/workflows/release.yml),
which builds the installer and attaches three assets to the release:
`AllTheThings_<version>_x64-setup.exe`, its `.sig` signature, and `latest.json`
(the updater manifest). Installed copies poll
`releases/latest/download/latest.json` and self-update. (You can also re-run the
workflow from the Actions tab via *Run workflow* against an existing tag.)

### Update signing

Updates are signed with a minisign keypair. The **public** key is baked into
`tauri.conf.json` (`plugins.updater.pubkey`); the **private** key is the
`TAURI_SIGNING_PRIVATE_KEY` GitHub Actions secret. The private key was generated
with `npx tauri signer generate` and lives at
`%USERPROFILE%\.allthethings-keys\updater.key` — **back it up** (e.g. a password
manager). If it is lost, you cannot ship updates that existing installs will
accept, and you'd have to re-key (which breaks the update path for already-installed copies).

## Architecture

Dependencies point inward; each module lives in exactly one layer.

| Layer | Path | Responsibility |
|-------|------|----------------|
| `domain` | `src-tauri/src/domain` | Pure value types: `FileEntry`, `RecordId`. No I/O. |
| `application` | `src-tauri/src/application` | `SearchIndex` (per volume), `Catalog` (all volumes), `Matcher`/`SearchOptions`, the `VolumeEnumerator` contract, status, errors. |
| `infrastructure` | `src-tauri/src/infrastructure` | `ntfs/`: `Volume` (raw reads), `MftReader` (MFT parse), `UsnWatcher` + catch-up (live updates), volume discovery; `cache`: on-disk index snapshots. |
| `presentation` | `src-tauri/src/presentation` | Tauri commands and managed state. |
| frontend | `src/` | Vanilla TS + Tailwind/DaisyUI, virtualized results list. |

The search index depends only on the `VolumeEnumerator` trait, so a non-NTFS or
test enumerator can be substituted without touching application logic.

## How it works

1. Volume discovery finds every fixed NTFS drive; each is scanned in turn.
2. `MftReader::open` reads the NTFS boot sector for geometry, then reads `$MFT`
   record 0 to follow the table's own data runs across the disk.
3. `enumerate` streams every in-use `FILE` record, applying update-sequence
   fixups and extracting the Win32 name(s), parent reference, size, the
   creation/modified/access timestamps and DOS attributes (from
   `$STANDARD_INFORMATION`) — one entry per hardlink path.
4. Each volume's `SearchIndex` holds entries in memory (with a precomputed
   name-sorted view for the instant browse-all default) and reconstructs full
   paths on demand by walking parent references to the root directory (record 5).
5. `Catalog` fans a query across volumes in parallel via a compiled `Matcher`
   and merges the ranked results.
6. A per-volume `UsnWatcher` tails the USN journal and applies
   create/delete/rename events to the live index, re-reading each changed
   record's MFT entry to fill in the size and timestamps the journal omits.
7. On startup, if a valid cached snapshot exists, it is loaded instantly and
   `catch_up` replays journal changes since the saved position; a full MFT scan
   runs only when the journal was recreated or its tail purged. The cache is
   refreshed periodically and right after each scan.

## Author & license

Built by **Swatto** ([@Swatto86](https://github.com/Swatto86)). Released under the
[MIT License](LICENSE).

AllTheThings is an independent reimplementation inspired by
[voidtools Everything](https://www.voidtools.com/); it shares no code with it.
