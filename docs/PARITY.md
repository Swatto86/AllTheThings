# Feature parity with voidtools Everything

A reference for what AllTheThings has versus [voidtools Everything](https://www.voidtools.com/)
(measured against Everything 1.4 / 1.5), and what's still on the table.

**Legend:** ✅ done · ⚠️ partial · ❌ not yet · ➖ out of scope (for now)

---

## Indexing engine

| Feature | Everything | AllTheThings | Notes |
|---|:---:|:---:|---|
| NTFS Master File Table indexing | ✅ | ✅ | Reads the raw `$MFT`. |
| USN change-journal live updates | ✅ | ✅ | Per-volume watcher. |
| Index all NTFS volumes | ✅ | ✅ | Discovered + merged. |
| Hardlink-aware (one entry per path) | ✅ | ✅ | |
| Persistent on-disk index cache | ✅ | ✅ | Loads instantly, replays the USN tail. |
| ReFS volumes | ✅ (1.5) | ❌ | NTFS only. |
| Folder indexing (FAT / network / removable) | ✅ | ❌ | Everything can index non-NTFS folders via a watcher. |
| Folder-size computation | ✅ | ❌ | |
| Index dates/attributes selectably | ✅ | ⚠️ | We always store size + modified time. |
| Include / exclude folders, hidden/system filter | ✅ | ❌ | We index everything. |

## Search syntax

| Feature | Everything | AllTheThings | Notes |
|---|:---:|:---:|---|
| Substring, search-as-you-type | ✅ | ✅ | |
| Multiple terms = AND | ✅ | ✅ | Space-separated. |
| Wildcards `*` `?` | ✅ | ✅ | |
| Regular expressions | ✅ | ✅ | Toggle. |
| Match case / whole word / path | ✅ | ✅ | Toggles. |
| `ext:`, `file:`, `folder:` | ✅ | ✅ | |
| `path:` (match against full path) | ✅ | ✅ | |
| `size:` with ranges + units | ✅ | ✅ | e.g. `size:>=1mb size:<16mb`. |
| OR `|`, NOT `!`, grouping `( )`, quotes `" "` | ✅ | ❌ | Only AND today. |
| Date filters `dm:` `dc:` `da:` | ✅ | ❌ | |
| Attribute filter `attrib:` | ✅ | ❌ | |
| Functions: `parent:` `child:` `count:` `dupe:` `len:` … | ✅ | ❌ | |
| Saved searches / macros | ✅ | ❌ | |
| Match diacritics | ✅ | ❌ | |
| Content search (`content:`) | ✅ | ❌ | |

## Results, columns & sorting

| Feature | Everything | AllTheThings | Notes |
|---|:---:|:---:|---|
| Virtualized result list (millions of rows) | ✅ | ✅ | |
| Result count + timing | ✅ | ✅ | |
| Shell file-type icons | ✅ | ✅ | Per extension, cached. |
| Columns: Name, Path, Size, Date Modified | ✅ | ✅ | |
| Sort by column (asc/desc) | ✅ | ✅ | Click header. |
| Resize columns | ✅ | ✅ | Drag the edge. |
| Reorder columns | ✅ | ✅ | Drag the header. |
| Add/remove columns (Created, Accessed, Type, Attributes, Ext, Run count…) | ✅ | ❌ | Fixed 4 columns. |
| Folders-first sorting | ✅ | ❌ | |
| Highlight matched text in results | ✅ | ❌ | |
| Thumbnail / large-icon views | ✅ | ❌ | Details view only. |
| Preview pane | ✅ | ❌ | |

## File actions & integration

| Feature | Everything | AllTheThings | Notes |
|---|:---:|:---:|---|
| Open (default app) | ✅ | ✅ | Double-click / Enter. |
| Open containing folder (selected in Explorer) | ✅ | ✅ | |
| Copy full path / file name | ✅ | ✅ | Right-click. |
| Full Explorer shell context menu | ✅ | ❌ | |
| Delete / rename / properties / run-as | ✅ | ❌ | |
| Drag & drop out to other apps | ✅ | ❌ | |

## System integration

| Feature | Everything | AllTheThings | Notes |
|---|:---:|:---:|---|
| System tray (minimize/close to tray) | ✅ | ✅ | |
| Run on startup | ✅ | ✅ | Elevated logon scheduled task. |
| Single instance | ✅ | ✅ | |
| Runs elevated for raw volume access | via service | ⚠️ | Everything's optional **service** lets the GUI run *without* admin; we require admin per launch (UAC), with the logon task providing silent elevation. |
| Explorer "Search Everything here" context menu | ✅ | ❌ | |
| Global hotkey to show | ✅ | ❌ | |

## Distribution & updates

| Feature | Everything | AllTheThings | Notes |
|---|:---:|:---:|---|
| Signed installer (NSIS) | ✅ | ✅ | Built in CI. |
| Auto-update | ✅ (1.5) | ✅ | Checks GitHub Releases, verifies signature, one-click install. |
| Portable mode | ✅ | ❌ | |
| Multi-language / localization | ✅ | ❌ | |
| Light/other themes | ✅ | ⚠️ | Dark only. |

## Power features

| Feature | Everything | AllTheThings | Notes |
|---|:---:|:---:|---|
| Bookmarks | ✅ | ❌ | |
| Search history | ✅ | ❌ | |
| Built-in / custom filters | ✅ | ❌ | Size presets only. |
| File lists (`.efu`) — create / open | ✅ | ❌ | |
| Export results (TXT / CSV / EFU) | ✅ | ❌ | |
| Command-line interface (`es.exe`) | ✅ | ❌ | |
| HTTP server (web UI) | ✅ | ❌ | |
| ETP/FTP server + client (remote search) | ✅ | ❌ | |
| IPC / SDK (other apps query the index) | ✅ | ❌ | |

---

## Suggested roadmap (rough priority)

1. **Boolean operators & quotes** — `OR` (`|`), `NOT` (`!`), grouping, `"exact phrase"`. High value, low cost.
2. **Date filters & more columns** — `dm:`/`dc:`/`da:`, plus Date Created / Type / Attributes columns (the MFT already has the data).
3. **Match highlighting** in results.
4. **Richer context menu** — Delete, Rename, Properties, full shell menu, drag-out.
5. **Folders-first sorting** and an add/remove-columns picker.
6. **Search history + bookmarks + saved filters.**
7. **Export** (CSV/TXT) and **`.efu` file lists.**
8. **Global hotkey** + Explorer "search here" integration.
9. **Light theme** and localization.
10. **Bigger lifts:** a real background **service** (so the GUI needn't be elevated), **folder/ReFS indexing**, a **CLI**, and an **HTTP/IPC** query interface.

> This list is a guide, not a commitment — pick what's useful. The core engine
> (instant MFT search, live USN updates, multi-volume, cache) is already at
> Everything's level; most gaps are UI/UX surface area and power-user tooling.
