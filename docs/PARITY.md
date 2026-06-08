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
| Index dates/attributes selectably | ✅ | ⚠️ | We always store size, modified, created, accessed + DOS attributes (not selectable). |
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
| OR `\|`, NOT `!`, quotes `" "` | ✅ | ✅ | |
| Grouping `( )` | ✅ | ✅ | Nestable; `a (b \| c)` = `a AND (b OR c)`. Quote a literal paren (`"(x86)"`). Unbalanced parens are tolerated. |
| Date filters `dm:` `dc:` `da:` | ✅ | ✅ | Keywords (`today`, `thisweek`…), `YYYY[-MM[-DD]]`, ranges `A..B`, `>`/`>=`/`<`/`<=`. |
| Attribute filter `attrib:` | ✅ | ✅ | e.g. `attrib:h`, `attrib:rhs` — all listed attributes must be set. |
| Functions: `parent:` `child:` `count:` `dupe:` `len:` … | ✅ | ❌ | |
| Saved searches / macros | ✅ | ❌ | |
| Match diacritics | ✅ | ❌ | |
| Content search (`content:`) | ✅ | ✅ | On-demand: the index narrows by filename, then candidate files' bodies are grepped GUI-side (your token; the service never reads contents). Text + BOM'd UTF-16, binary skipped, cancellable. |

## Results, columns & sorting

| Feature | Everything | AllTheThings | Notes |
|---|:---:|:---:|---|
| Virtualized result list (millions of rows) | ✅ | ✅ | |
| Result count + timing | ✅ | ✅ | |
| Shell file-type icons | ✅ | ✅ | Per extension, cached. |
| Columns: Name, Path, Size, Date Modified | ✅ | ✅ | |
| Sort by column (asc/desc) | ✅ | ✅ | Click header — Name/Path/Size/Modified/Created/Accessed/Ext/Attributes (Type not sortable yet). |
| Resize columns | ✅ | ✅ | Drag the edge. |
| Reorder columns | ✅ | ✅ | Drag the header. |
| Add/remove columns (Created, Accessed, Type, Attributes, Ext, Run count…) | ✅ | ⚠️ | Header right-click picker: Created / Accessed / Type / Ext / Attributes (no Run count). Layout persisted. |
| Folders-first sorting | ✅ | ✅ | Toolbar 📁 toggle; folders grouped first in any sort. |
| Highlight matched text in results | ✅ | ✅ | Name + Path. |
| Thumbnail / large-icon views | ✅ | ❌ | Details view only. |
| Preview pane | ✅ | ❌ | |

## File actions & integration

| Feature | Everything | AllTheThings | Notes |
|---|:---:|:---:|---|
| Open (default app) | ✅ | ✅ | Double-click / Enter. |
| Open containing folder (selected in Explorer) | ✅ | ✅ | |
| Copy full path / file name | ✅ | ✅ | Right-click. |
| Full Explorer shell context menu | ✅ | ⚠️ | Discrete shell verbs (Properties, Open with, Run as admin) via the menu; full `IContextMenu` not yet. |
| Delete / rename / properties / run-as | ✅ | ✅ | Delete → Recycle Bin (confirmed), inline Rename (F2), Properties, Run as admin, Open with. |
| Drag & drop out to other apps | ✅ | ❌ | |

## System integration

| Feature | Everything | AllTheThings | Notes |
|---|:---:|:---:|---|
| System tray (minimize/close to tray) | ✅ | ✅ | |
| Run on startup | ✅ | ✅ | Elevated logon scheduled task. |
| Single instance | ✅ | ✅ | |
| GUI runs **without** elevation | ✅ (via service) | ✅ | The LocalSystem **service** (`AllTheThingsSvc`) indexes; the GUI runs `asInvoker` and queries it over a query-only pipe. Admin is requested (UAC) only to install/manage the service, with the elevated logon task as an in-process fallback. |
| Explorer "Search ... here" context menu | ✅ | ✅ | Per-user (HKCU) static verb; on Windows 11 under "Show more options". |
| Global hotkey to show | ✅ | ✅ | Configurable in Settings (default Ctrl+Alt+Space); toggles the window. |

## Distribution & updates

| Feature | Everything | AllTheThings | Notes |
|---|:---:|:---:|---|
| Signed installer (NSIS) | ✅ | ✅ | Built in CI. |
| Auto-update | ✅ (1.5) | ✅ | Checks GitHub Releases, verifies signature, one-click install. |
| Portable mode | ✅ | ❌ | |
| Multi-language / localization | ✅ | ❌ | |
| Light/other themes | ✅ | ✅ | Light + dark, or follow Windows (Settings). |

## Power features

| Feature | Everything | AllTheThings | Notes |
|---|:---:|:---:|---|
| Bookmarks | ✅ | ❌ | |
| Search history | ✅ | ✅ | Recent queries dropdown. |
| Built-in / custom filters | ✅ | ❌ | Size presets only. |
| File lists (`.efu`) — create / open | ✅ | ⚠️ | Can **write** `.efu`; opening an `.efu` as a source not yet. |
| Export results (TXT / CSV / EFU) | ✅ | ✅ | Toolbar **Export** → Save dialog; format from the chosen extension. |
| Command-line interface (`es.exe`) | ✅ | ❌ | |
| HTTP server (web UI) | ✅ | ❌ | |
| ETP/FTP server + client (remote search) | ✅ | ❌ | |
| IPC / SDK (other apps query the index) | ✅ | ⚠️ | Internal query-only named pipe (`\\.\pipe\AllTheThings`) backs the optional service; no public/documented SDK yet. |

---

## Suggested roadmap (rough priority)

1. **Full shell context menu** (`IContextMenu`) and **drag & drop out** — discrete verbs (Delete, Rename, Properties, Open with, Run as admin) are done.
2. **Bookmarks & saved filters** (search history is done).
3. **Open `.efu` file lists** as a search source (writing them is done).
4. **Localization.**
5. **Bigger lifts:** **folder/ReFS indexing**, a **CLI**, an **HTTP** query interface, and a public IPC/SDK.

_Recently shipped: **query grouping** `( )` (nestable; `a (b \| c)` = `a AND (b OR c)`); **light/dark/system theming**; a configurable **global hotkey** (summon/hide from anywhere) and an Explorer **"Search here"** folder context-menu entry; **the GUI runs without admin** — a LocalSystem background **service** indexes and the `asInvoker` GUI queries it over a query-only pipe (installer registers + starts the service; Settings manages it via an elevated relaunch), export results to CSV / TXT / EFU, folders-first sorting and sortable Ext / Attributes columns, right-click file actions (Delete to Recycle Bin, inline Rename/F2, Properties, Open with, Run as admin), date filters `dm:`/`dc:`/`da:` and `attrib:`, Created / Accessed / Type / Ext / Attributes columns with a header column picker, live USN updates now carry full metadata (size + all timestamps), boolean `\|`/`!`/`"…"`, match highlighting, search history._

> This list is a guide, not a commitment — pick what's useful. The core engine
> (instant MFT search, live USN updates, multi-volume, cache) is already at
> Everything's level; most gaps are UI/UX surface area and power-user tooling.
