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
- **Query syntax**: space-separated AND terms, `*`/`?` wildcards, and operators `ext:`, `path:`, `file:`, `folder:`, `size:` (e.g. `ext:dll size:>100mb`, `report*.pdf`).
- **Toggles** (toolbar): match case, whole word, regular expression, match full path.
- **Size filter** dropdown: Empty / Tiny / Small / Medium / Large / Huge / Gigantic presets (inserts the matching `size:` range).
- **Shell file-type icons** — the real Windows icon per extension/folder, fetched on demand and cached.
- **Sortable, resizable, reorderable columns** — click a header to sort (again to reverse), drag the edge to resize, drag the header to reorder.
- **Right-click**: open, open containing folder (Explorer with the item selected), copy full path, copy name.
- **Single-instance**: launching again focuses the running window.
- **System tray**: minimise/close to tray; Show / Settings / Quit menu; left-click to restore.
- **Run at startup** (Settings): registers an elevated logon scheduled task so it auto-starts with admin rights into the tray and indexes in the background — no UAC prompt. The installer sets this up too.
- **Settings**: start-with-Windows and close-to-tray toggles, persisted to `%LOCALAPPDATA%\AllTheThings\settings.json`.
- **Keyboard**: type to filter, ↑/↓ to move, Enter to open.

## Status

Working build with daily-driver parity. Files created after the initial scan show a blank size until the next full index (the USN journal carries no size field).

## Download & install

Grab the latest installer from the [**Releases**](https://github.com/Swatto86/AllTheThings/releases) page (`AllTheThings_<version>_x64-setup.exe`) and run it — it requests Administrator and registers an elevated logon task so the app auto-starts into the system tray and indexes in the background. Uninstalling removes the task.

Releases are built automatically by the [release workflow](.github/workflows/release.yml) whenever a `v*` tag is pushed.

## Requirements

- Windows with at least one **NTFS** volume
- **Administrator** rights (raw volume access is privileged)
- Rust (pinned to 1.95.0 via `rust-toolchain.toml`) and Node 20+

## Verify the engine (no GUI)

The MFT/USN engine has a live integration test, ignored by default. Run it from an **elevated** terminal:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml -- --ignored --nocapture --test-threads=1
```

It indexes every NTFS volume and checks hardlink paths, wildcards, and the `ext:`/`folder:`/`size:` operators.

## Run

```powershell
npm install
npm run tauri dev      # launch the app (run the terminal As Administrator)
```

If not elevated, the status bar shows an access-denied error and results stay empty.

## Build an installer

```powershell
npm run tauri build    # produces an NSIS installer under src-tauri/target/release/bundle
```

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
   fixups and extracting the Win32 name(s), parent reference, size, and
   timestamp — one entry per hardlink path.
4. Each volume's `SearchIndex` holds entries in memory (with a precomputed
   name-sorted view for the instant browse-all default) and reconstructs full
   paths on demand by walking parent references to the root directory (record 5).
5. `Catalog` fans a query across volumes in parallel via a compiled `Matcher`
   and merges the ranked results.
6. A per-volume `UsnWatcher` tails the USN journal and applies
   create/delete/rename events to the live index.
7. On startup, if a valid cached snapshot exists, it is loaded instantly and
   `catch_up` replays journal changes since the saved position; a full MFT scan
   runs only when the journal was recreated or its tail purged. The cache is
   refreshed periodically and right after each scan.

## Author & license

Built by **Swatto** ([@Swatto86](https://github.com/Swatto86)). Released under the
[MIT License](LICENSE).

AllTheThings is an independent reimplementation inspired by
[voidtools Everything](https://www.voidtools.com/); it shares no code with it.
