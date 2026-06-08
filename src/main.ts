import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { save } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { getVersion } from "@tauri-apps/api/app";
import "./styles.css";

const WEBSITE_URL = "https://swatto.co.uk";

// ---- Backend contract (mirrors src-tauri presentation DTOs) ----
interface Hit {
  name: string;
  path: string;
  size: number; // bytes; -1 for directories/unknown
  modified: number; // unix millis; 0 if unknown
  created: number; // unix millis; 0 if unknown
  accessed: number; // unix millis; 0 if unknown
  attributes: number; // FILE_ATTRIBUTE_* bitmask
  isDir: boolean;
}
interface SearchResponse {
  total: number;
  tookMs: number;
  hits: Hit[];
  error?: string;
  capped?: boolean; // content search: candidate pool truncated (partial scan)
}
interface IndexStatus {
  state: "indexing" | "ready" | "error";
  count: number;
  volume: string;
  message: string;
}

type SortKey = "name" | "path" | "size" | "modified" | "created" | "accessed" | "ext" | "attributes";
interface SearchOptions {
  query: string;
  limit: number;
  matchCase: boolean;
  wholeWord: boolean;
  regex: boolean;
  matchPath: boolean;
  sort: SortKey;
  ascending: boolean;
  foldersFirst: boolean;
}

type ColKey =
  | "name"
  | "path"
  | "size"
  | "date"
  | "created"
  | "accessed"
  | "type"
  | "ext"
  | "attributes";
interface Column {
  key: ColKey;
  label: string;
  /** Backend sort column, or `null` for client-only columns (no sorting). */
  sort: SortKey | null;
  width: number;
  flex: boolean;
}

const ROW_HEIGHT = 26;
const RESULT_LIMIT = 5000;

const SIZE_PRESETS: [string, string][] = [
  ["Any size", ""],
  ["Empty (0)", "size:0"],
  ["Tiny (≤ 10 KB)", "size:<=10kb"],
  ["Small (10 KB – 100 KB)", "size:>=10kb size:<100kb"],
  ["Medium (100 KB – 1 MB)", "size:>=100kb size:<1mb"],
  ["Large (1 MB – 16 MB)", "size:>=1mb size:<16mb"],
  ["Huge (16 MB – 128 MB)", "size:>=16mb size:<128mb"],
  ["Gigantic (> 128 MB)", "size:>128mb"],
];

const FOLDERS_FIRST_KEY = "att.foldersFirst";
function loadFoldersFirst(): boolean {
  try {
    return localStorage.getItem(FOLDERS_FIRST_KEY) === "1";
  } catch {
    return false;
  }
}

const options: SearchOptions = {
  query: "",
  limit: RESULT_LIMIT,
  matchCase: false,
  wholeWord: false,
  regex: false,
  matchPath: false,
  sort: "name",
  ascending: true,
  foldersFirst: loadFoldersFirst(),
};

// Every column the UI can show. The active set (which, in what order, at what
// width) is user-chosen via the header right-click picker and persisted.
const ALL_COLUMNS: readonly Column[] = [
  { key: "name", label: "Name", sort: "name", width: 340, flex: false },
  { key: "path", label: "Path", sort: "path", width: 0, flex: true },
  { key: "size", label: "Size", sort: "size", width: 96, flex: false },
  { key: "date", label: "Date modified", sort: "modified", width: 160, flex: false },
  { key: "created", label: "Date created", sort: "created", width: 160, flex: false },
  { key: "accessed", label: "Date accessed", sort: "accessed", width: 160, flex: false },
  { key: "type", label: "Type", sort: null, width: 150, flex: false },
  { key: "ext", label: "Ext", sort: "ext", width: 70, flex: false },
  { key: "attributes", label: "Attributes", sort: "attributes", width: 96, flex: false },
];
const DEFAULT_COLUMNS: ColKey[] = ["name", "path", "size", "date"];
const COLUMNS_KEY = "att.columns";

let columns: Column[] = loadColumns();

function colTemplate(key: ColKey): Column {
  return { ...ALL_COLUMNS.find((c) => c.key === key)! };
}

function loadColumns(): Column[] {
  try {
    const saved: unknown = JSON.parse(localStorage.getItem(COLUMNS_KEY) ?? "null");
    if (Array.isArray(saved)) {
      const cols: Column[] = [];
      for (const s of saved as { key: ColKey; width?: number }[]) {
        if (cols.some((c) => c.key === s.key)) continue;
        const base = ALL_COLUMNS.find((c) => c.key === s.key);
        if (base) cols.push({ ...base, width: typeof s.width === "number" ? s.width : base.width });
      }
      if (cols.some((c) => c.key === "name")) return cols;
    }
  } catch {
    /* fall through to defaults */
  }
  return DEFAULT_COLUMNS.map(colTemplate);
}

function saveColumns(): void {
  try {
    localStorage.setItem(COLUMNS_KEY, JSON.stringify(columns.map((c) => ({ key: c.key, width: c.width }))));
  } catch {
    /* storage unavailable */
  }
}

let hits: Hit[] = [];
let total = 0;
let selected = -1;
let searchSeq = 0;
let debounce: number | undefined;
let highlightTerms: string[] = [];
let resizing = false;
let dragSrc: ColKey | null = null;
let renamingIndex = -1;
// The Hit being renamed, captured by reference so a commit always targets the
// right file even if a background search replaces `hits` while the box is open.
let renamingHit: Hit | null = null;
let suppressScrollCancel = false;

const iconCache = new Map<string, string>(); // key -> "data:..." | "none"
const iconPending = new Set<string>();
let iconRerenderQueued = false;

const app = document.querySelector<HTMLDivElement>("#app")!;
app.innerHTML = /* html */ `
  <div class="flex flex-col h-screen bg-base-100 text-base-content">
    <div id="update-banner" class="update-banner hidden">
      <span id="update-text"></span>
      <div class="flex gap-2 ml-auto">
        <button id="update-install" class="btn btn-xs btn-primary">Install &amp; restart</button>
        <button id="update-later" class="btn btn-xs btn-ghost">Later</button>
      </div>
    </div>
    <div id="service-banner" class="update-banner hidden">
      <span id="service-banner-text">AllTheThings can now index in the background without admin. Install the service?</span>
      <div class="flex gap-2 ml-auto">
        <button id="service-banner-install" class="btn btn-xs btn-primary">Install service</button>
        <button id="service-banner-later" class="btn btn-xs btn-ghost">Not now</button>
      </div>
    </div>
    <div class="flex items-center gap-2 p-2 border-b border-base-300">
      <button id="brand" title="About AllTheThings" class="toolbar-brand">
        <span class="brand-name">AllTheThings</span>
        <span id="brand-version" class="brand-version"></span>
      </button>
      <input id="q" type="text" placeholder="Search all the things…" autocomplete="off" spellcheck="false"
        class="input input-bordered input-sm flex-1 font-mono" />
      <div class="relative">
        <button id="sizebtn" title="Filter by size" class="btn btn-xs">Size ▾</button>
        <div id="sizemenu" class="menu-pop hidden"></div>
      </div>
      <div class="relative">
        <button id="optbtn" title="Match options" class="btn btn-xs">Options ▾</button>
        <div id="opt-menu" class="menu-pop hidden"></div>
      </div>
      <button id="builder" title="Search builder — compose a query from fields" class="btn btn-xs">Builder</button>
      <button id="folders-first" title="Folders first" class="btn btn-xs">📁</button>
      <button id="export" title="Export results (CSV / TXT / EFU)" class="btn btn-xs">Export</button>
      <button id="gear" title="Settings" class="btn btn-xs">⚙</button>
      <div id="count" class="text-xs opacity-60 whitespace-nowrap min-w-[130px] text-right"></div>
    </div>
    <div id="head" class="row !h-7 font-semibold text-xs opacity-70 border-b border-base-300 bg-base-200"></div>
    <div id="viewport" class="flex-1 overflow-y-auto scroll-thin relative">
      <div id="spacer"></div>
      <div id="rows" class="absolute top-0 left-0 right-0"></div>
    </div>
    <div id="status" class="text-xs px-2 py-1 border-t border-base-300 bg-base-200 opacity-80"></div>
  </div>
  <div id="menu" class="menu-pop hidden"></div>
  <div id="col-menu" class="menu-pop hidden"></div>
  <div id="history-menu" class="menu-pop hidden"></div>
  <div id="settings-overlay" class="overlay hidden">
    <div class="settings-panel">
      <div class="settings-title">Settings</div>
      <label class="settings-row">
        <span>Theme<br><span class="hint">Light, dark, or follow Windows</span></span>
        <select id="set-theme" class="select select-bordered select-sm">
          <option value="business">Dark</option>
          <option value="corporate">Light</option>
          <option value="system">System</option>
        </select>
      </label>
      <label class="settings-row">
        <span>Start with Windows<br><span class="hint">Launches the app into the tray at sign-in so search is ready</span></span>
        <input type="checkbox" id="set-startup" class="toggle toggle-sm toggle-primary" />
      </label>
      <div class="settings-row">
        <span>Background index service<br><span class="hint" id="svc-hint">A Windows service that indexes for the app, so it can run without elevation later</span></span>
        <div class="svc-controls">
          <span id="svc-state" class="svc-state">…</span>
          <button id="svc-power" class="btn btn-xs hidden"></button>
          <button id="svc-install" class="btn btn-xs hidden"></button>
        </div>
      </div>
      <label class="settings-row">
        <span>Close button minimizes to tray<br><span class="hint">Otherwise the window closing quits the app</span></span>
        <input type="checkbox" id="set-tray" class="toggle toggle-sm toggle-primary" />
      </label>
      <div class="settings-row">
        <span>Global hotkey<br><span class="hint" id="hotkey-hint">Click the box and press a key combo to summon the window from anywhere</span></span>
        <div class="svc-controls">
          <input type="text" id="hotkey-input" class="hotkey-input" readonly placeholder="Click & press keys" />
          <button id="hotkey-clear" class="btn btn-xs hidden" title="Disable the global hotkey">Disable</button>
        </div>
      </div>
      <label class="settings-row">
        <span>Explorer right-click "Search here"<br><span class="hint">Adds "Search AllTheThings here" to folder menus (under "Show more options" on Windows 11)</span></span>
        <input type="checkbox" id="set-explorer" class="toggle toggle-sm toggle-primary" />
      </label>
      <div class="settings-row">
        <span>Updates<br><span class="hint" id="update-status">AllTheThings checks for updates on launch.</span></span>
        <button id="check-updates" class="btn btn-sm">Check now</button>
      </div>
      <div id="settings-msg" class="settings-msg"></div>
      <div class="settings-actions"><button id="settings-close" class="btn btn-sm">Close</button></div>
    </div>
  </div>
  <div id="builder-overlay" class="overlay hidden">
    <div class="settings-panel builder-panel">
      <div class="settings-title">Search builder</div>
      <label class="settings-row">
        <span>Name contains<br><span class="hint">Words in the file or folder name (space = AND, <code>|</code> = OR)</span></span>
        <input type="text" id="b-name" class="input input-bordered input-sm builder-input" autocomplete="off" spellcheck="false" placeholder="e.g. report" />
      </label>
      <label class="settings-row">
        <span>Contents contain<br><span class="hint">Search inside text files — slower; binaries skipped</span></span>
        <input type="text" id="b-content" class="input input-bordered input-sm builder-input" autocomplete="off" spellcheck="false" placeholder="e.g. timeout" />
      </label>
      <label class="settings-row">
        <span>Extensions<br><span class="hint">Space or comma separated, e.g. <code>dll exe png</code></span></span>
        <input type="text" id="b-ext" class="input input-bordered input-sm builder-input" autocomplete="off" spellcheck="false" placeholder="e.g. pdf docx" />
      </label>
      <label class="settings-row">
        <span>Size</span>
        <select id="b-size" class="select select-bordered select-sm"></select>
      </label>
      <div class="settings-row">
        <span>Date</span>
        <div class="svc-controls">
          <select id="b-date-field" class="select select-bordered select-sm">
            <option value="dm">Modified</option>
            <option value="dc">Created</option>
            <option value="da">Accessed</option>
          </select>
          <select id="b-date-range" class="select select-bordered select-sm">
            <option value="">Any time</option>
            <option value="today">Today</option>
            <option value="yesterday">Yesterday</option>
            <option value="thisweek">This week</option>
            <option value="lastweek">Last week</option>
            <option value="thismonth">This month</option>
            <option value="lastmonth">Last month</option>
            <option value="thisyear">This year</option>
            <option value="lastyear">Last year</option>
          </select>
        </div>
      </div>
      <label class="settings-row">
        <span>Show</span>
        <select id="b-type" class="select select-bordered select-sm">
          <option value="">Files &amp; folders</option>
          <option value="file:">Files only</option>
          <option value="folder:">Folders only</option>
        </select>
      </label>
      <div class="settings-row">
        <span>Attributes<br><span class="hint">Match only items with every ticked attribute</span></span>
        <div class="builder-chips" id="b-attrs"></div>
      </div>
      <div class="settings-row">
        <span>Match<br><span class="hint">How name &amp; path terms are compared — not file contents</span></span>
        <div class="builder-chips" id="b-match"></div>
      </div>
      <div class="settings-row builder-preview-row">
        <span>Query preview</span>
        <code id="b-preview" class="builder-preview"></code>
      </div>
      <div id="builder-msg" class="settings-msg"></div>
      <div class="settings-actions builder-actions">
        <button id="b-reset" class="btn btn-sm btn-ghost">Reset</button>
        <div class="builder-action-group">
          <button id="b-cancel" class="btn btn-sm">Close</button>
          <button id="b-search" class="btn btn-sm btn-primary">Search</button>
        </div>
      </div>
    </div>
  </div>
  <div id="confirm-overlay" class="overlay hidden">
    <div class="settings-panel" style="width:380px">
      <div class="settings-title" id="confirm-title">Delete</div>
      <div id="confirm-msg" class="confirm-msg"></div>
      <div class="settings-actions" style="gap:8px">
        <button id="confirm-cancel" class="btn btn-sm">Cancel</button>
        <button id="confirm-ok" class="btn btn-sm btn-error">Delete</button>
      </div>
    </div>
  </div>
  <div id="about-overlay" class="overlay hidden">
    <div class="settings-panel about-panel">
      <div class="about-body">
        <div class="about-logo">🔎</div>
        <div class="about-name">AllTheThings</div>
        <div id="about-version" class="about-version"></div>
        <div class="about-tagline">Instant file search for Windows</div>
      </div>
      <div class="settings-row">
        <span>Developer</span>
        <span>Swatto</span>
      </div>
      <div class="settings-row">
        <span>Website</span>
        <a id="about-website" href="https://swatto.co.uk" class="about-link">swatto.co.uk</a>
      </div>
      <div class="settings-actions"><button id="about-close" class="btn btn-sm">Close</button></div>
    </div>
  </div>
  <input id="rename-input" class="rename-input hidden" spellcheck="false" autocomplete="off" />
`;

const q = document.querySelector<HTMLInputElement>("#q")!;
const countEl = document.querySelector<HTMLDivElement>("#count")!;
const viewport = document.querySelector<HTMLDivElement>("#viewport")!;
const spacer = document.querySelector<HTMLDivElement>("#spacer")!;
const rows = document.querySelector<HTMLDivElement>("#rows")!;
const statusEl = document.querySelector<HTMLDivElement>("#status")!;
const head = document.querySelector<HTMLDivElement>("#head")!;
const menu = document.querySelector<HTMLDivElement>("#menu")!;
const colMenu = document.querySelector<HTMLDivElement>("#col-menu")!;
const confirmOverlay = document.querySelector<HTMLDivElement>("#confirm-overlay")!;
const confirmMsg = document.querySelector<HTMLDivElement>("#confirm-msg")!;
const confirmOk = document.querySelector<HTMLButtonElement>("#confirm-ok")!;
const confirmCancel = document.querySelector<HTMLButtonElement>("#confirm-cancel")!;
const renameInput = document.querySelector<HTMLInputElement>("#rename-input")!;
const sizeBtn = document.querySelector<HTMLButtonElement>("#sizebtn")!;
const sizeMenu = document.querySelector<HTMLDivElement>("#sizemenu")!;
const optBtn = document.querySelector<HTMLButtonElement>("#optbtn")!;
const optMenu = document.querySelector<HTMLDivElement>("#opt-menu")!;
const historyMenu = document.querySelector<HTMLDivElement>("#history-menu")!;
const foldersFirstBtn = document.querySelector<HTMLButtonElement>("#folders-first")!;
const exportBtn = document.querySelector<HTMLButtonElement>("#export")!;
const gear = document.querySelector<HTMLButtonElement>("#gear")!;
const brand = document.querySelector<HTMLButtonElement>("#brand")!;
const brandVersion = document.querySelector<HTMLSpanElement>("#brand-version")!;
const aboutOverlay = document.querySelector<HTMLDivElement>("#about-overlay")!;
const aboutVersion = document.querySelector<HTMLDivElement>("#about-version")!;
const aboutWebsite = document.querySelector<HTMLAnchorElement>("#about-website")!;
const aboutClose = document.querySelector<HTMLButtonElement>("#about-close")!;
const settingsOverlay = document.querySelector<HTMLDivElement>("#settings-overlay")!;
const setStartup = document.querySelector<HTMLInputElement>("#set-startup")!;
const setTray = document.querySelector<HTMLInputElement>("#set-tray")!;
const setExplorer = document.querySelector<HTMLInputElement>("#set-explorer")!;
const setTheme = document.querySelector<HTMLSelectElement>("#set-theme")!;
const hotkeyInput = document.querySelector<HTMLInputElement>("#hotkey-input")!;
const hotkeyClear = document.querySelector<HTMLButtonElement>("#hotkey-clear")!;
const hotkeyHint = document.querySelector<HTMLSpanElement>("#hotkey-hint")!;
const HOTKEY_HINT = "Click the box and press a key combo to summon the window from anywhere";
const settingsMsg = document.querySelector<HTMLDivElement>("#settings-msg")!;
const settingsClose = document.querySelector<HTMLButtonElement>("#settings-close")!;
const svcState = document.querySelector<HTMLSpanElement>("#svc-state")!;
const svcPower = document.querySelector<HTMLButtonElement>("#svc-power")!;
const svcInstall = document.querySelector<HTMLButtonElement>("#svc-install")!;
const svcHint = document.querySelector<HTMLSpanElement>("#svc-hint")!;

/// Whether this session's searches are served by the background service (chosen
/// once at launch). Drives the status-bar source indicator and the Settings note.
let backendUsesService = false;
const updateBanner = document.querySelector<HTMLDivElement>("#update-banner")!;
const updateText = document.querySelector<HTMLSpanElement>("#update-text")!;
const updateInstall = document.querySelector<HTMLButtonElement>("#update-install")!;
const updateLater = document.querySelector<HTMLButtonElement>("#update-later")!;
const serviceBanner = document.querySelector<HTMLDivElement>("#service-banner")!;
const serviceBannerText = document.querySelector<HTMLSpanElement>("#service-banner-text")!;
const serviceBannerInstall = document.querySelector<HTMLButtonElement>("#service-banner-install")!;
const serviceBannerLater = document.querySelector<HTMLButtonElement>("#service-banner-later")!;
const checkUpdates = document.querySelector<HTMLButtonElement>("#check-updates")!;
const updateStatus = document.querySelector<HTMLSpanElement>("#update-status")!;

// ---- Formatting ----
function fmtSize(bytes: number, isDir: boolean): string {
  if (isDir || bytes < 0) return "";
  const u = ["B", "KB", "MB", "GB", "TB"];
  let n = bytes;
  let i = 0;
  while (n >= 1024 && i < u.length - 1) {
    n /= 1024;
    i++;
  }
  return `${i === 0 ? n : n.toFixed(1)} ${u[i]}`;
}

function fmtDate(ms: number): string {
  if (!ms) return "";
  const d = new Date(ms);
  const p = (x: number) => x.toString().padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

function dirOf(path: string): string {
  const i = path.lastIndexOf("\\");
  return i > 0 ? path.slice(0, i) : path;
}

function extOf(name: string): string {
  const i = name.lastIndexOf(".");
  return i > 0 ? name.slice(i + 1).toLowerCase() : "";
}

// FILE_ATTRIBUTE_* bits → Explorer-style letters, in display order.
const ATTR_LETTERS: [number, string][] = [
  [0x1, "R"], // readonly
  [0x2, "H"], // hidden
  [0x4, "S"], // system
  [0x20, "A"], // archive
  [0x10, "D"], // directory
  [0x400, "L"], // reparse point
  [0x200, "P"], // sparse
  [0x800, "C"], // compressed
  [0x4000, "E"], // encrypted
  [0x100, "T"], // temporary
  [0x1000, "O"], // offline
  [0x2000, "I"], // not content indexed
];

function fmtAttribs(attrs: number): string {
  let s = "";
  for (const [bit, ch] of ATTR_LETTERS) if (attrs & bit) s += ch;
  return s;
}

function esc(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]!);
}

// Whether the query asks to search inside file contents.
const HAS_CONTENT = /(^|\s)content:/i;
// Remove `content:term` / `content:"phrase"` so the rest can be highlighted /
// routed as a filename query (the content term matches bodies, not names).
function stripContent(query: string): string {
  return query
    .replace(/(^|\s)content:("[^"]*"?|\S*)/gi, "$1")
    .replace(/\s+/g, " ")
    .trim();
}

type QToken =
  | { kind: "word" | "phrase"; text: string }
  | { kind: "or" | "not" | "lparen" | "rparen" };

// Tokenize a query the same way the Rust matcher does (application/search.rs):
// `"…"` is a phrase, `|`/`(`/`)` are always operators, and `!` is NOT only at
// the start of a term (a mid-word `!` is literal). Kept in lockstep with the
// backend so highlighting reflects what actually matched.
function tokenizeQuery(query: string): QToken[] {
  const tokens: QToken[] = [];
  let buf = "";
  const flush = (): void => {
    if (buf) {
      tokens.push({ kind: "word", text: buf });
      buf = "";
    }
  };
  for (let i = 0; i < query.length; i++) {
    const c = query[i];
    if (c === '"') {
      flush();
      let phrase = "";
      i++;
      while (i < query.length && query[i] !== '"') phrase += query[i++];
      if (phrase) tokens.push({ kind: "phrase", text: phrase });
    } else if (c === "|") {
      flush();
      tokens.push({ kind: "or" });
    } else if (c === "(") {
      flush();
      tokens.push({ kind: "lparen" });
    } else if (c === ")") {
      flush();
      tokens.push({ kind: "rparen" });
    } else if (c === "!" && buf === "") {
      tokens.push({ kind: "not" });
    } else if (/\s/.test(c)) {
      flush();
    } else {
      buf += c;
    }
  }
  flush();
  return tokens;
}

// Plain terms to highlight: quoted phrases and bare words, minus operators,
// ext:/size:/… functions, and wildcards. Cosmetic only — the authoritative
// matcher is the Rust query parser. It tracks negation parity and group scope
// so a term inside a negated group (`!(a | b)`, `report !old`) is NOT marked
// (`!!x` cancels back to positive, mirroring the backend).
function computeHighlightTerms(query: string, regex: boolean): string[] {
  if (regex) return [];
  const terms: string[] = [];
  let negated = false; // cumulative NOT parity at the current point
  let pending = false; // a NOT awaiting the next term or group
  const stack: boolean[] = []; // saved parity per open `(`
  for (const t of tokenizeQuery(query)) {
    switch (t.kind) {
      case "not":
        pending = !pending;
        break;
      case "or":
        pending = false; // a dangling `!` before `|` negates nothing
        break;
      case "lparen":
        stack.push(negated);
        negated = negated !== pending; // the group inherits the pending NOT
        pending = false;
        break;
      case "rparen":
        if (stack.length) negated = stack.pop() as boolean;
        pending = false;
        break;
      case "phrase": {
        const neg = negated !== pending;
        pending = false;
        if (!neg && t.text) terms.push(t.text.toLowerCase());
        break;
      }
      case "word": {
        const neg = negated !== pending;
        pending = false;
        if (neg) break;
        let tok = t.text;
        const lower = tok.toLowerCase();
        if (/^(ext|size|file|files|folder|folders|dir|dm|dc|da|attrib):/.test(lower)) break;
        if (lower.startsWith("path:")) tok = tok.slice(5);
        if (!tok || tok.includes("*") || tok.includes("?")) break;
        terms.push(tok.toLowerCase());
        break;
      }
    }
  }
  return terms;
}

// Escape `raw` and wrap any matched term occurrences in <mark>.
function highlight(raw: string): string {
  if (!highlightTerms.length) return esc(raw);
  const lower = raw.toLowerCase();
  // toLowerCase can change UTF-16 length (e.g. İ U+0130 -> "i̇"), which
  // would desync the lowercased match indices from the original string and mark
  // the wrong characters. Such names are rare — just skip highlighting them.
  if (lower.length !== raw.length) return esc(raw);
  const ranges: [number, number][] = [];
  for (const t of highlightTerms) {
    let i = lower.indexOf(t);
    while (i !== -1) {
      ranges.push([i, i + t.length]);
      i = lower.indexOf(t, i + t.length);
    }
  }
  if (!ranges.length) return esc(raw);
  ranges.sort((a, b) => a[0] - b[0]);
  const merged: [number, number][] = [];
  for (const r of ranges) {
    const last = merged[merged.length - 1];
    if (last && r[0] <= last[1]) last[1] = Math.max(last[1], r[1]);
    else merged.push([r[0], r[1]]);
  }
  let out = "";
  let pos = 0;
  for (const [s, e] of merged) {
    out += esc(raw.slice(pos, s)) + `<mark class="hl">${esc(raw.slice(s, e))}</mark>`;
    pos = e;
  }
  return out + esc(raw.slice(pos));
}

// ---- Icons ----
function iconKey(h: Hit): string {
  return h.isDir ? "dir" : extOf(h.name) || "file";
}

function ensureIcon(h: Hit): void {
  const key = iconKey(h);
  if (iconCache.has(key) || iconPending.has(key)) return;
  iconPending.add(key);
  invoke<string | null>("file_icon", { ext: h.isDir ? null : extOf(h.name), isDir: h.isDir })
    .then((b64) => {
      iconCache.set(key, b64 ? `data:image/png;base64,${b64}` : "none");
      if (b64) queueIconRerender();
    })
    .catch(() => iconCache.set(key, "none"))
    .finally(() => iconPending.delete(key));
}

function queueIconRerender(): void {
  if (iconRerenderQueued) return;
  iconRerenderQueued = true;
  requestAnimationFrame(() => {
    iconRerenderQueued = false;
    renderVisible();
  });
}

function iconHtml(h: Hit): string {
  const url = iconCache.get(iconKey(h));
  if (url && url.startsWith("data:")) {
    return `<span class="cell-icon" style="background-image:url('${url}')"></span>`;
  }
  ensureIcon(h);
  return `${h.isDir ? "📁" : "📄"} `;
}

// ---- Type names (registry-resolved, cached per extension like icons) ----
const typeCache = new Map<string, string>(); // key -> friendly type name
const typePending = new Set<string>();

function typeKey(h: Hit): string {
  return h.isDir ? "dir" : extOf(h.name) || "file";
}

function ensureType(h: Hit): void {
  const key = typeKey(h);
  if (typeCache.has(key) || typePending.has(key)) return;
  typePending.add(key);
  invoke<string | null>("file_type", { ext: h.isDir ? null : extOf(h.name), isDir: h.isDir })
    .then((t) => {
      typeCache.set(key, t ?? "");
      if (t) queueIconRerender();
    })
    .catch(() => typeCache.set(key, ""))
    .finally(() => typePending.delete(key));
}

function typeLabel(h: Hit): string {
  const cached = typeCache.get(typeKey(h));
  if (cached !== undefined) return cached;
  ensureType(h);
  return ""; // filled in on the next render once resolved
}

// ---- Table ----
function setCols(): void {
  const tmpl = columns.map((c) => (c.flex ? "minmax(140px,1fr)" : `${c.width}px`)).join(" ");
  app.style.setProperty("--cols", tmpl);
}

function cellHtml(h: Hit, key: ColKey): string {
  switch (key) {
    case "name":
      return `<div title="${esc(h.name)}">${iconHtml(h)}${highlight(h.name)}</div>`;
    case "path":
      return `<div class="opacity-70" title="${esc(h.path)}">${highlight(dirOf(h.path))}</div>`;
    case "size":
      return `<div class="text-right pr-2 opacity-80">${fmtSize(h.size, h.isDir)}</div>`;
    case "date":
      return `<div class="opacity-70">${fmtDate(h.modified)}</div>`;
    case "created":
      return `<div class="opacity-70">${fmtDate(h.created)}</div>`;
    case "accessed":
      return `<div class="opacity-70">${fmtDate(h.accessed)}</div>`;
    case "type":
      return `<div class="opacity-70" title="${esc(typeLabel(h))}">${esc(typeLabel(h))}</div>`;
    case "ext":
      return `<div class="opacity-70">${esc(h.isDir ? "" : extOf(h.name))}</div>`;
    case "attributes":
      return `<div class="opacity-70 font-mono">${fmtAttribs(h.attributes)}</div>`;
  }
}

function renderHeader(): void {
  head.innerHTML = columns
    .map((c) => {
      const ind = c.sort && c.sort === options.sort ? (options.ascending ? " ▲" : " ▼") : "";
      const grip = c.flex ? "" : `<span class="grip" data-grip="${c.key}"></span>`;
      const align = c.key === "size" ? "text-right pr-2" : "";
      const sortable = c.sort ? "" : " not-sortable";
      return `<div data-col="${c.key}" draggable="true" class="${align}${sortable}">${c.label}<span class="ind">${ind}</span>${grip}</div>`;
    })
    .join("");
  wireHeader();
}

function renderVisible(): void {
  const scrollTop = viewport.scrollTop;
  const first = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - 8);
  const last = Math.min(hits.length, Math.ceil((scrollTop + viewport.clientHeight) / ROW_HEIGHT) + 8);

  let html = "";
  for (let i = first; i < last; i++) {
    const h = hits[i];
    const sel = i === selected ? " selected" : "";
    const cells = columns.map((c) => cellHtml(h, c.key)).join("");
    html += `<div class="row${sel}" style="position:absolute;top:${i * ROW_HEIGHT}px;left:0;right:0" data-i="${i}">${cells}</div>`;
  }
  rows.innerHTML = html;
}

// ---- Column resize / reorder ----
function wireHeader(): void {
  head.querySelectorAll<HTMLElement>("[data-col]").forEach((cell) => {
    const col = columns.find((c) => c.key === (cell.dataset.col as ColKey))!;

    cell.addEventListener("click", (e) => {
      if ((e.target as HTMLElement).classList.contains("grip") || resizing) return;
      if (!col.sort) return; // client-only column — not sortable
      if (options.sort === col.sort) options.ascending = !options.ascending;
      else {
        options.sort = col.sort;
        options.ascending = true;
      }
      renderHeader();
      runSearch();
    });

    cell.addEventListener("dragstart", () => {
      dragSrc = col.key;
      cell.classList.add("dragging");
    });
    cell.addEventListener("dragend", () => {
      cell.classList.remove("dragging");
      head.querySelectorAll(".drag-over").forEach((x) => x.classList.remove("drag-over"));
    });
    cell.addEventListener("dragover", (e) => {
      e.preventDefault();
      cell.classList.add("drag-over");
    });
    cell.addEventListener("dragleave", () => cell.classList.remove("drag-over"));
    cell.addEventListener("drop", (e) => {
      e.preventDefault();
      cell.classList.remove("drag-over");
      if (dragSrc && dragSrc !== col.key) moveColumn(dragSrc, col.key);
      dragSrc = null;
    });
  });

  head.querySelectorAll<HTMLElement>(".grip").forEach((grip) => {
    grip.addEventListener("pointerdown", (e) => startResize(e, grip.dataset.grip as ColKey));
  });
}

function moveColumn(src: ColKey, target: ColKey): void {
  const from = columns.findIndex((c) => c.key === src);
  const to = columns.findIndex((c) => c.key === target);
  if (from < 0 || to < 0) return;
  const [col] = columns.splice(from, 1);
  columns.splice(to, 0, col);
  saveColumns();
  setCols();
  renderHeader();
  renderVisible();
}

function startResize(e: PointerEvent, key: ColKey): void {
  e.preventDefault();
  e.stopPropagation();
  const col = columns.find((c) => c.key === key);
  if (!col) return;
  resizing = true;
  const startX = e.clientX;
  const startW = col.width;
  const move = (ev: PointerEvent) => {
    col.width = Math.max(60, startW + (ev.clientX - startX));
    setCols();
  };
  const up = () => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
    saveColumns();
    setTimeout(() => (resizing = false), 0);
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
}

// ---- Column picker (header right-click) ----
function showColumnPicker(x: number, y: number): void {
  refreshColumnPicker();
  colMenu.style.left = `${Math.min(x, window.innerWidth - 220)}px`;
  colMenu.style.top = `${Math.min(y, window.innerHeight - ALL_COLUMNS.length * 30)}px`;
  colMenu.classList.remove("hidden");
}

function refreshColumnPicker(): void {
  const visible = new Set(columns.map((c) => c.key));
  colMenu.innerHTML = ALL_COLUMNS.map((c) => {
    const on = visible.has(c.key);
    const locked = c.key === "name";
    return `<button data-ck="${c.key}"${locked ? " disabled" : ""}><span class="chk">${on ? "✓" : ""}</span>${esc(c.label)}</button>`;
  }).join("");
  colMenu.querySelectorAll<HTMLButtonElement>("button").forEach((b) => {
    b.onclick = (e) => {
      // refreshColumnPicker() replaces these buttons, detaching the click
      // target; without this the bubbling click would hit the window dismiss
      // handler and close the picker after a single toggle.
      e.stopPropagation();
      toggleColumn(b.dataset.ck as ColKey);
    };
  });
}

function toggleColumn(key: ColKey): void {
  if (key === "name") return; // Name is mandatory.
  const idx = columns.findIndex((c) => c.key === key);
  let sortChanged = false;
  if (idx >= 0) {
    // Hiding the active sort column falls back to Name-ascending.
    if (columns[idx].sort && columns[idx].sort === options.sort) {
      options.sort = "name";
      options.ascending = true;
      sortChanged = true;
    }
    columns.splice(idx, 1);
  } else {
    columns.push(colTemplate(key));
  }
  saveColumns();
  setCols();
  renderHeader();
  refreshColumnPicker();
  if (sortChanged) runSearch();
  else renderVisible();
}

// ---- Search ----
async function runSearch(): Promise<void> {
  const seq = ++searchSeq;
  const isContent = HAS_CONTENT.test(options.query);
  // Highlight only the filename terms; the content term matches bodies, not names.
  highlightTerms = computeHighlightTerms(stripContent(options.query), options.regex);
  try {
    // Content search reads files, so it's slower — show that it's working.
    if (isContent) countEl.textContent = "Searching file contents…";
    const res = await invoke<SearchResponse>(isContent ? "search_content" : "search", { options });
    if (seq !== searchSeq) return; // superseded
    if (res.error) {
      countEl.textContent = "";
      statusEl.textContent = `Query error: ${res.error}`;
      return;
    }
    hits = res.hits;
    total = res.total;
    selected = -1;
    spacer.style.height = `${hits.length * ROW_HEIGHT}px`;
    viewport.scrollTop = 0;
    const partial = isContent && res.capped ? " (first 50,000 scanned)" : "";
    countEl.textContent = `${total.toLocaleString()}${isContent ? " in files" : " found"}${partial} · ${res.tookMs} ms`;
    renderVisible();
  } catch (e) {
    if (seq === searchSeq) statusEl.textContent = `Search error: ${e}`;
  }
}

function scheduleSearch(): void {
  window.clearTimeout(debounce);
  debounce = window.setTimeout(runSearch, 60);
}

async function pollStatus(): Promise<void> {
  try {
    const s = await invoke<IndexStatus>("index_status");
    if (s.state === "error") {
      // An error already names the source (e.g. "background service unavailable"),
      // so don't append the "· via service" suffix and contradict it.
      statusEl.textContent = `Index error: ${s.message}`;
    } else {
      const src = backendUsesService ? " · via service" : "";
      statusEl.textContent =
        (s.state === "indexing"
          ? `Indexing ${s.volume}… ${s.count.toLocaleString()} items`
          : `Ready · ${s.count.toLocaleString()} items on ${s.volume}`) + src;
    }
    if (s.state === "indexing") {
      window.setTimeout(pollStatus, 350);
    } else {
      runSearch();
    }
  } catch (e) {
    statusEl.textContent = `Backend unavailable: ${e}`;
  }
}

// ---- Context menu ----
function hideMenu(): void {
  menu.classList.add("hidden");
}

const RUNAS_EXTS = ["exe", "msi", "bat", "cmd", "com", "ps1", "scr"];
type MenuRow = "sep" | [string, () => void];

function showMenu(x: number, y: number, h: Hit): void {
  const ext = h.isDir ? "" : extOf(h.name);
  const rows: MenuRow[] = [
    ["Open", () => invoke("open_path", { path: h.path }).catch(reportErr)],
    ["Open containing folder", () => invoke("reveal_path", { path: h.path }).catch(reportErr)],
  ];
  if (!h.isDir) {
    rows.push(["Open with…", () => invoke("shell_action", { path: h.path, action: "open_with" }).catch(reportErr)]);
  }
  if (RUNAS_EXTS.includes(ext)) {
    rows.push(["Run as administrator", () => invoke("shell_action", { path: h.path, action: "run_as" }).catch(reportErr)]);
  }
  rows.push(
    "sep",
    ["Copy full path", () => copy(h.path)],
    ["Copy name", () => copy(h.name)],
    "sep",
    ["Rename", () => startRename()],
    ["Delete", () => deleteSelected()],
    "sep",
    ["Properties", () => invoke("shell_action", { path: h.path, action: "properties" }).catch(reportErr)],
  );

  const actions: (() => void)[] = [];
  menu.innerHTML = rows
    .map((r) => {
      if (r === "sep") return `<div class="sep"></div>`;
      const i = actions.push(r[1]) - 1;
      return `<button data-mi="${i}">${esc(r[0])}</button>`;
    })
    .join("");
  menu.querySelectorAll<HTMLButtonElement>("button").forEach((b) => {
    b.onclick = () => {
      hideMenu();
      actions[Number(b.dataset.mi)]();
    };
  });
  menu.style.left = `${Math.min(x, window.innerWidth - 220)}px`;
  menu.style.top = `${Math.min(y, window.innerHeight - rows.length * 28)}px`;
  menu.classList.remove("hidden");
}

// ---- File actions: rename (inline), delete (to Recycle Bin) ----
function startRename(): void {
  if (selected < 0 || selected >= hits.length) return;
  const h = hits[selected];
  // The row may have been scrolled out of the virtualized window; bring it back
  // and re-render so its cell exists before we measure it.
  const top = selected * ROW_HEIGHT;
  if (top < viewport.scrollTop || top + ROW_HEIGHT > viewport.scrollTop + viewport.clientHeight) {
    suppressScrollCancel = true;
    viewport.scrollTop = top < viewport.scrollTop ? top : top + ROW_HEIGHT - viewport.clientHeight;
    renderVisible();
  }
  const rowEl = rows.querySelector<HTMLElement>(`[data-i="${selected}"]`);
  const nameIdx = columns.findIndex((c) => c.key === "name");
  const cell = rowEl?.children[nameIdx] as HTMLElement | undefined;
  if (!cell) return;

  const rect = cell.getBoundingClientRect();
  renameInput.value = h.name;
  renameInput.style.left = `${rect.left}px`;
  renameInput.style.top = `${rect.top}px`;
  renameInput.style.width = `${rect.width}px`;
  renameInput.style.height = `${rect.height}px`;
  renameInput.classList.remove("hidden");
  renameInput.focus();
  const dot = h.name.lastIndexOf(".");
  renameInput.setSelectionRange(0, dot > 0 ? dot : h.name.length);
  renamingIndex = selected;
  renamingHit = h;
}

function cancelRename(): void {
  if (renamingIndex < 0) return;
  renamingIndex = -1;
  renamingHit = null;
  renameInput.classList.add("hidden");
}

async function commitRename(restoreFocus: boolean): Promise<void> {
  if (renamingIndex < 0) return;
  // Operate on the captured Hit, not hits[renamingIndex]: a background search may
  // have replaced `hits` while the box was open, so the stale index could point
  // at a different file (wrong-file rename) or be out of range (TypeError).
  const h = renamingHit;
  const newName = renameInput.value.trim();
  cancelRename();
  // On Enter, return focus to the search box so keyboard nav keeps working; on
  // blur, leave focus wherever the user clicked.
  if (restoreFocus) q.focus();
  if (!h || !newName || newName === h.name) return;
  try {
    const newPath = await invoke<string>("rename_path", { path: h.path, newName });
    h.name = newName;
    h.path = newPath;
    renderVisible();
  } catch (e) {
    reportErr(e);
  }
}

async function deleteSelected(): Promise<void> {
  if (selected < 0 || selected >= hits.length || confirmResolve !== null) return;
  const h = hits[selected];
  const seq = searchSeq;
  if (!(await confirmDelete(h.name))) return;
  try {
    await invoke("delete_path", { path: h.path });
    // A background search may have replaced `hits` (and reset `selected`) while
    // the confirm dialog was open. If so it already reflects current state —
    // don't splice by the stale index, which would drop the wrong row and skew
    // the count. Otherwise remove the row by identity, not the cached index.
    if (seq !== searchSeq) return;
    const idx = hits.indexOf(h);
    if (idx < 0) return;
    hits.splice(idx, 1);
    total = Math.max(0, total - 1);
    if (selected >= hits.length) selected = hits.length - 1;
    spacer.style.height = `${hits.length * ROW_HEIGHT}px`;
    countEl.textContent = `${total.toLocaleString()} found`;
    renderVisible();
  } catch (e) {
    reportErr(e);
  }
}

// ---- Confirm dialog ----
let confirmResolve: ((ok: boolean) => void) | null = null;

function confirmDelete(name: string): Promise<boolean> {
  confirmMsg.textContent = `Move "${name}" to the Recycle Bin?`;
  confirmOverlay.classList.remove("hidden");
  confirmOk.focus();
  return new Promise((resolve) => {
    confirmResolve = resolve;
  });
}

function resolveConfirm(ok: boolean): void {
  confirmOverlay.classList.add("hidden");
  confirmResolve?.(ok);
  confirmResolve = null;
}

async function copy(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    statusEl.textContent = "Clipboard unavailable";
  }
}

function reportErr(e: unknown): void {
  statusEl.textContent = `Action failed: ${e}`;
}

// ---- Size dropdown ----
function buildSizeMenu(): void {
  sizeMenu.innerHTML = SIZE_PRESETS.map(([label], i) => `<button data-si="${i}">${esc(label)}</button>`).join("");
  sizeMenu.querySelectorAll<HTMLButtonElement>("button").forEach((b, i) => {
    b.onclick = () => {
      sizeMenu.classList.add("hidden");
      applySizePreset(SIZE_PRESETS[i][1]);
    };
  });
}

function applySizePreset(expr: string): void {
  const base = q.value
    .split(/\s+/)
    .filter((t) => t && !t.toLowerCase().startsWith("size:"))
    .join(" ");
  const next = expr ? `${base} ${expr}`.trim() : base;
  q.value = next;
  options.query = next;
  runSearch();
  q.focus();
}

// ---- Search builder ----
// A friendly form that composes a query string from fields (name, contents,
// extensions, size, date, type, attributes) so the syntax is discoverable —
// in particular it surfaces content search, which has no toolbar button.
const builderOverlay = document.querySelector<HTMLDivElement>("#builder-overlay")!;
const builderBtn = document.querySelector<HTMLButtonElement>("#builder")!;
const bName = document.querySelector<HTMLInputElement>("#b-name")!;
const bContent = document.querySelector<HTMLInputElement>("#b-content")!;
const bExt = document.querySelector<HTMLInputElement>("#b-ext")!;
const bSize = document.querySelector<HTMLSelectElement>("#b-size")!;
const bDateField = document.querySelector<HTMLSelectElement>("#b-date-field")!;
const bDateRange = document.querySelector<HTMLSelectElement>("#b-date-range")!;
const bType = document.querySelector<HTMLSelectElement>("#b-type")!;
const bAttrs = document.querySelector<HTMLDivElement>("#b-attrs")!;
const bMatch = document.querySelector<HTMLDivElement>("#b-match")!;
const bPreview = document.querySelector<HTMLElement>("#b-preview")!;
const bSearch = document.querySelector<HTMLButtonElement>("#b-search")!;
const bCancel = document.querySelector<HTMLButtonElement>("#b-cancel")!;
const bReset = document.querySelector<HTMLButtonElement>("#b-reset")!;

// [letter, label] for the `attrib:` builder. `attrib:` requires ALL listed bits.
const BUILDER_ATTRS: [string, string][] = [
  ["h", "Hidden"],
  ["s", "System"],
  ["r", "Read-only"],
  ["a", "Archive"],
  ["c", "Compressed"],
  ["e", "Encrypted"],
];
// Match toggles the builder can set — the subset of MATCH_OPTIONS that composes
// with functions. Regex is omitted: it treats the whole query as one pattern,
// which would break the composed `ext:`/`size:`/… functions.
const BUILDER_MATCH: [MatchKey, string][] = [
  ["matchCase", "Case"],
  ["wholeWord", "Whole word"],
  ["matchPath", "Full path"],
];

function buildBuilderControls(): void {
  bSize.innerHTML = SIZE_PRESETS.map(
    ([label, expr]) => `<option value="${esc(expr)}">${esc(label)}</option>`,
  ).join("");
  bAttrs.innerHTML = BUILDER_ATTRS.map(
    ([letter, label]) =>
      `<label class="builder-chip"><input type="checkbox" data-attr="${letter}" />${esc(label)}</label>`,
  ).join("");
  bMatch.innerHTML = BUILDER_MATCH.map(
    ([key, label]) =>
      `<label class="builder-chip"><input type="checkbox" data-match="${key}" />${esc(label)}</label>`,
  ).join("");
  builderOverlay
    .querySelectorAll<HTMLInputElement>('input[data-attr], input[data-match]')
    .forEach((el) => el.addEventListener("change", updateBuilderPreview));
}

// Build a `content:` token, quoting the phrase when it has spaces. Embedded
// quotes are stripped so they can't break the wrapper.
function contentToken(raw: string): string {
  const t = raw.replace(/"/g, "").trim();
  if (!t) return "";
  return /\s/.test(t) ? `content:"${t}"` : `content:${t}`;
}

// Function prefixes that would turn a bare Name word into a metadata filter
// instead of a literal name match.
const FN_PREFIX = /^(ext|size|content|dm|dc|da|attrib|path|file|files|folder|folders|dir):/i;

// Turn one Name word into a query term. Words containing grouping/negation
// characters or a function prefix are quoted so they match literally (the
// builder has dedicated fields for those); ordinary words — and wildcards —
// stay bare so the match options still apply to them.
function nameWord(word: string): string {
  const clean = word.replace(/"/g, "");
  if (!clean) return "";
  if (/[()]/.test(clean) || clean.startsWith("!") || FN_PREFIX.test(clean)) {
    return `"${clean}"`;
  }
  return clean;
}

// Compose the Name field into a query fragment: words are AND-ed, `|` is OR.
// Multiple OR alternatives are parenthesised so the other builder filters bind
// to the whole name expression, not just the last alternative.
function nameToQuery(raw: string): string {
  const segments = raw
    .split("|")
    .map((seg) => seg.trim().split(/\s+/).map(nameWord).filter(Boolean).join(" "))
    .filter(Boolean);
  if (segments.length === 0) return "";
  if (segments.length === 1) return segments[0];
  return `(${segments.join(" | ")})`;
}

// Normalise one Extensions entry to a bare extension: take the segment after the
// last dot (so `*.dll` / `tar.gz` → `dll` / `gz`) and drop anything that isn't a
// valid extension character. The backend's `ext:` only compares the final
// segment, so a compound or glob would otherwise never match.
function normalizeExt(token: string): string {
  const segments = token.split(".").filter(Boolean);
  const last = (segments.pop() ?? "").toLowerCase();
  // Strip only characters illegal in Windows filenames (which also covers the
  // query operators " : ? * | …); legitimate ext chars like `+` (c++) and
  // non-ASCII letters survive, so a real extension is never silently rewritten.
  return last.replace(/[<>:"/\\|?*]/g, "");
}

function composeBuilderQuery(): string {
  const parts: string[] = [];

  const name = nameToQuery(bName.value);
  if (name) parts.push(name);

  const exts = [...new Set(bExt.value.split(/[\s,;|()]+/).map(normalizeExt).filter(Boolean))];
  if (exts.length) parts.push(`ext:${exts.join(";")}`);

  // A size filter only narrows files; combining it with "Folders only" can never
  // match, so drop it there (the control is greyed out to match).
  if (bSize.value && bType.value !== "folder:") parts.push(bSize.value);
  if (bDateRange.value) parts.push(`${bDateField.value}:${bDateRange.value}`);
  if (bType.value) parts.push(bType.value);

  const letters = Array.from(bAttrs.querySelectorAll<HTMLInputElement>("input"))
    .filter((el) => el.checked)
    .map((el) => el.dataset.attr)
    .join("");
  if (letters) parts.push(`attrib:${letters}`);

  const content = contentToken(bContent.value);
  if (content) parts.push(content);

  return parts.join(" ");
}

// Size can't apply to folders, so grey out the control under "Folders only".
function syncBuilderSize(): void {
  bSize.disabled = bType.value === "folder:";
}

function updateBuilderPreview(): void {
  bPreview.textContent = composeBuilderQuery() || "(empty — matches everything)";
}

function builderMatchInput(key: MatchKey): HTMLInputElement | null {
  return bMatch.querySelector<HTMLInputElement>(`input[data-match="${key}"]`);
}

// Clear every query-composing field (match chips are synced separately).
function clearBuilderFields(): void {
  bName.value = "";
  bContent.value = "";
  bExt.value = "";
  bSize.value = "";
  bDateField.value = "dm";
  bDateRange.value = "";
  bType.value = "";
  bAttrs.querySelectorAll<HTMLInputElement>("input").forEach((el) => (el.checked = false));
}

// Reflect the live match options onto the chips so they round-trip.
function syncBuilderMatch(): void {
  for (const [key] of BUILDER_MATCH) {
    const el = builderMatchInput(key);
    if (el) el.checked = options[key];
  }
}

function openBuilder(): void {
  // A fresh form every open — fields cleared, chips synced to the live options —
  // so the modal is never a confusing mix of stale and current state.
  closeSettings();
  clearBuilderFields();
  syncBuilderMatch();
  syncBuilderSize();
  updateBuilderPreview();
  builderOverlay.classList.remove("hidden");
  bName.focus();
}

function closeBuilder(): void {
  builderOverlay.classList.add("hidden");
}

function applyBuilder(): void {
  const query = composeBuilderQuery();
  for (const [key] of BUILDER_MATCH) {
    const el = builderMatchInput(key);
    if (el) options[key] = el.checked;
  }
  // The builder emits standard syntax (ext:/size:/…), which regex mode would
  // treat as one literal pattern — so clear it.
  options.regex = false;
  q.value = query;
  options.query = query;
  syncControls();
  closeBuilder();
  runSearch();
  q.focus();
}

function resetBuilder(): void {
  clearBuilderFields();
  syncBuilderMatch();
  syncBuilderSize();
  updateBuilderPreview();
  bName.focus();
}

// ---- Search history ----
const HISTORY_KEY = "att.history";
const HISTORY_MAX = 25;
let history: string[] = loadHistory();

function loadHistory(): string[] {
  try {
    const v = JSON.parse(localStorage.getItem(HISTORY_KEY) ?? "[]");
    return Array.isArray(v) ? v : [];
  } catch {
    return [];
  }
}

function addHistory(query: string): void {
  const v = query.trim();
  if (!v) return;
  history = [v, ...history.filter((h) => h !== v)].slice(0, HISTORY_MAX);
  try {
    localStorage.setItem(HISTORY_KEY, JSON.stringify(history));
  } catch {
    /* storage unavailable */
  }
}

function showHistory(): void {
  if (q.value.trim() || !history.length) {
    hideHistory();
    return;
  }
  const items = history.map((h, i) => `<button data-hi="${i}">${esc(h)}</button>`).join("");
  historyMenu.innerHTML = `${items}<div class="sep"></div><button id="history-clear" class="history-clear">Clear history</button>`;
  historyMenu.querySelectorAll<HTMLButtonElement>("button[data-hi]").forEach((b) => {
    const i = Number(b.dataset.hi);
    b.addEventListener("mousedown", (e) => e.preventDefault()); // keep input focus
    b.addEventListener("click", () => {
      q.value = history[i];
      options.query = history[i];
      hideHistory();
      runSearch();
      q.focus();
    });
  });
  const clear = historyMenu.querySelector<HTMLButtonElement>("#history-clear")!;
  clear.addEventListener("mousedown", (e) => e.preventDefault()); // keep input focus
  clear.addEventListener("click", clearHistory);
  const r = q.getBoundingClientRect();
  historyMenu.style.left = `${r.left}px`;
  historyMenu.style.top = `${r.bottom + 2}px`;
  historyMenu.style.minWidth = `${r.width}px`;
  historyMenu.classList.remove("hidden");
}

function hideHistory(): void {
  historyMenu.classList.add("hidden");
}

function clearHistory(): void {
  history = [];
  try {
    localStorage.removeItem(HISTORY_KEY);
  } catch {
    /* storage unavailable */
  }
  hideHistory();
}

// The search match-modes, shown as a ticked checklist in the Options dropdown.
type MatchKey = "matchCase" | "wholeWord" | "regex" | "matchPath";
const MATCH_OPTIONS: [MatchKey, string][] = [
  ["matchCase", "Match case"],
  ["wholeWord", "Match whole word"],
  ["regex", "Regular expression"],
  ["matchPath", "Match full path"],
];

// Reflect the current options on the toolbar button (highlight + count) and the
// dropdown's ticks. Called at init and whenever an option changes.
function syncControls(): void {
  const active = MATCH_OPTIONS.filter(([k]) => options[k]).length;
  optBtn.classList.toggle("btn-primary", active > 0);
  optBtn.textContent = active > 0 ? `Options (${active}) ▾` : "Options ▾";
  buildOptMenu();
}

function buildOptMenu(): void {
  optMenu.innerHTML = MATCH_OPTIONS.map(
    ([key, label]) =>
      `<button data-mk="${key}"><span class="chk">${options[key] ? "✓" : ""}</span>${esc(label)}</button>`,
  ).join("");
  optMenu.querySelectorAll<HTMLButtonElement>("button").forEach((b) => {
    b.onclick = (e) => {
      // Keep the menu open so several options can be toggled; stop the bubble so
      // the window dismiss handler doesn't close it.
      e.stopPropagation();
      const key = b.dataset.mk as MatchKey;
      options[key] = !options[key];
      syncControls();
      runSearch();
    };
  });
}

function syncFoldersFirst(): void {
  foldersFirstBtn.classList.toggle("btn-primary", options.foldersFirst);
}

// ---- Theme ----
// "business" = dark (default), "corporate" = light, "system" follows Windows.
// Stored in localStorage so it applies instantly (the inline script in index.html
// sets it before first paint to avoid a flash).
const THEME_KEY = "att.theme";
function themePref(): string {
  try {
    return localStorage.getItem(THEME_KEY) || "business";
  } catch {
    return "business";
  }
}
function resolveTheme(pref: string): string {
  if (pref === "system") {
    return window.matchMedia?.("(prefers-color-scheme: dark)")?.matches ? "business" : "corporate";
  }
  return pref === "corporate" ? "corporate" : "business";
}
function applyTheme(): void {
  document.documentElement.dataset.theme = resolveTheme(themePref());
}

// ---- Settings ----
interface Settings {
  closeToTray: boolean;
  runAtStartup: boolean;
  explorerMenu: boolean;
  hotkey: string;
}

async function loadSettings(): Promise<void> {
  try {
    const s = await invoke<Settings>("get_settings");
    setStartup.checked = s.runAtStartup;
    setTray.checked = s.closeToTray;
    setExplorer.checked = s.explorerMenu;
    hotkeyInput.value = s.hotkey;
    syncHotkeyControls();
    // Reconcile from reality: a stored hotkey that didn't bind (another app owns
    // it) is flagged rather than shown as silently working.
    const active = await invoke<boolean>("hotkey_active").catch(() => true);
    hotkeyHint.textContent =
      s.hotkey && !active
        ? "Inactive — another app may already use this combo; pick another."
        : HOTKEY_HINT;
    settingsMsg.textContent = "";
  } catch (e) {
    settingsMsg.textContent = `Could not load settings: ${e}`;
  }
}

async function saveSettings(): Promise<void> {
  settingsMsg.textContent = "";
  try {
    await invoke("set_settings", {
      settings: {
        runAtStartup: setStartup.checked,
        closeToTray: setTray.checked,
        explorerMenu: setExplorer.checked,
        hotkey: hotkeyInput.value, // ignored by the backend; owned by set_hotkey
      },
    });
  } catch (e) {
    settingsMsg.textContent = `${e}`;
    await loadSettings(); // re-sync toggles with reality (e.g. task creation failed)
  }
}

// ---- Global hotkey capture ----
// Map a KeyboardEvent.code to a Tauri accelerator key name, or null if it isn't
// a usable hotkey key.
function hotkeyKeyName(code: string): string | null {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  if (code === "Space") return "Space";
  const named: Record<string, string> = {
    ArrowUp: "Up", ArrowDown: "Down", ArrowLeft: "Left", ArrowRight: "Right",
    Enter: "Enter", Tab: "Tab", Home: "Home", End: "End",
    PageUp: "PageUp", PageDown: "PageDown", Insert: "Insert", Delete: "Delete",
  };
  return named[code] ?? null;
}

// Build "Ctrl+Alt+Space"-style accelerators. Requires at least one modifier — a
// bare key would be captured system-wide.
// Note: the Windows key is intentionally not offered — the plugin's parser
// rejects a "Win" token, and most Win combos are reserved by the OS anyway.
function accelFromEvent(e: KeyboardEvent): string | null {
  const key = hotkeyKeyName(e.code);
  if (!key) return null;
  const mods: string[] = [];
  if (e.ctrlKey) mods.push("Ctrl");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");
  return mods.length ? [...mods, key].join("+") : null;
}

async function setHotkey(accel: string): Promise<void> {
  settingsMsg.textContent = "";
  try {
    await invoke("set_hotkey", { hotkey: accel });
    hotkeyInput.value = accel;
    hotkeyHint.textContent = HOTKEY_HINT; // just registered (or disabled) successfully
    syncHotkeyControls();
  } catch (e) {
    settingsMsg.textContent = `${e}`;
    await loadSettings(); // revert the box to the still-registered hotkey
  }
}

// The "Disable" button is an action, not a status — only show it when a hotkey
// is actually set, so an empty box reads clearly as "off".
function syncHotkeyControls(): void {
  hotkeyClear.classList.toggle("hidden", !hotkeyInput.value.trim());
}

// ---- Explorer "Search here" ----
// Scope the view to a folder (from the right-click context menu). Uses
// match-path mode + a quoted path phrase so it works for paths with spaces; the
// user can then append a filename to filter within the folder.
function searchHere(path: string): void {
  if (!path) return;
  const folder = path.replace(/[\\/]+$/, "") + "\\";
  // The scope query is a literal quoted path phrase, so force literal match-path
  // mode regardless of the current toggles (regex would fail to compile it).
  options.matchPath = true;
  options.regex = false;
  options.wholeWord = false;
  syncControls();
  q.value = `"${folder}" `;
  options.query = q.value;
  runSearch();
  getCurrentWindow().show().catch(() => {});
  getCurrentWindow().setFocus().catch(() => {});
  q.focus();
  q.setSelectionRange(q.value.length, q.value.length);
}

function openSettings(): void {
  closeBuilder(); // the two modals are mutually exclusive
  setTheme.value = themePref();
  loadSettings();
  fetchServiceState();
  settingsOverlay.classList.remove("hidden");
}

function closeSettings(): void {
  settingsOverlay.classList.add("hidden");
}

function openAbout(): void {
  closeSettings();
  closeBuilder();
  aboutOverlay.classList.remove("hidden");
}

function closeAbout(): void {
  aboutOverlay.classList.add("hidden");
}

// Pull the app version (from tauri.conf.json) into the toolbar brand and the
// About dialog so both stay in sync with the release without hard-coding.
getVersion()
  .then((v) => {
    brandVersion.textContent = `v${v}`;
    aboutVersion.textContent = `Version ${v}`;
  })
  .catch(() => {});

// ---- Background service ----
type SvcState =
  | "not_installed"
  | "stopped"
  | "running"
  | "start_pending"
  | "stop_pending"
  | "other";

// Guards against overlapping install/start/stop actions.
let svcBusy = false;

/// Read live SCM state + which backend this session uses, and render the row.
async function fetchServiceState(): Promise<SvcState | "error"> {
  try {
    const [state, usesService, elevated] = await Promise.all([
      invoke<SvcState>("service_status"),
      invoke<boolean>("uses_service"),
      invoke<boolean>("is_elevated"),
    ]);
    backendUsesService = usesService;
    renderServiceState(state, usesService, elevated);
    return state;
  } catch (e) {
    svcState.textContent = "unavailable";
    svcPower.classList.add("hidden");
    svcInstall.classList.add("hidden");
    settingsMsg.textContent = `Service status unavailable: ${e}`;
    return "error";
  }
}

function renderServiceState(state: SvcState, usesService: boolean, elevated: boolean): void {
  const pending = state === "start_pending" || state === "stop_pending";
  svcInstall.classList.remove("hidden");
  svcInstall.disabled = svcBusy || pending;
  svcPower.disabled = svcBusy || pending;

  // The install button toggles install/uninstall; the power button start/stop.
  const setInstall = (label: string, action: string) => {
    svcInstall.textContent = label;
    svcInstall.dataset.action = action;
  };
  const setPower = (label: string | null, action?: string) => {
    if (label === null) {
      svcPower.classList.add("hidden");
      return;
    }
    svcPower.classList.remove("hidden");
    svcPower.textContent = label;
    svcPower.dataset.action = action!;
  };

  switch (state) {
    case "not_installed":
      svcState.textContent = "Not installed";
      setInstall("Install", "install");
      setPower(null);
      break;
    case "running":
      svcState.textContent = usesService ? "Running · in use" : "Running";
      setInstall("Uninstall", "uninstall");
      setPower("Stop", "stop");
      break;
    case "stopped":
      svcState.textContent = "Installed · stopped";
      setInstall("Uninstall", "uninstall");
      setPower("Start", "start");
      break;
    case "start_pending":
      svcState.textContent = "Starting…";
      setInstall("Uninstall", "uninstall");
      setPower(null);
      break;
    case "stop_pending":
      svcState.textContent = "Stopping…";
      setInstall("Uninstall", "uninstall");
      setPower(null);
      break;
    default:
      svcState.textContent = "Installed";
      setInstall("Uninstall", "uninstall");
      setPower(null);
  }

  // The backend is fixed at launch and never silently re-indexes locally, so the
  // hint must track BOTH the session binding (usesService) and the live state —
  // otherwise stopping/uninstalling the in-use service shows "served by the
  // service" next to a "stopped" label.
  if (usesService && state === "running") {
    svcHint.textContent = "Search is served by the background service.";
  } else if (usesService) {
    svcHint.textContent = "The service this session was using is no longer running — restart AllTheThings.";
  } else if (state !== "not_installed") {
    svcHint.textContent = "Installed — restart AllTheThings to search via the service.";
  } else {
    svcHint.textContent = "A Windows service that indexes for the app, so it can run without admin.";
  }
  // When the GUI is unelevated, managing the service triggers a UAC prompt.
  if (!elevated) {
    svcHint.textContent += " Managing it prompts for admin.";
  }
}

/// Invoke a service command, then settle the UI on the resulting SCM state
/// (start/stop briefly report a *_pending state).
async function serviceAction(command: string): Promise<void> {
  if (svcBusy) return;
  svcBusy = true;
  settingsMsg.textContent = "";
  svcInstall.disabled = true;
  svcPower.disabled = true;
  try {
    await invoke(command);
  } catch (e) {
    settingsMsg.textContent = `${e}`;
  }
  svcBusy = false;
  for (let i = 0; i < 12; i++) {
    if (settingsOverlay.classList.contains("hidden")) break;
    const state = await fetchServiceState();
    if (state !== "start_pending" && state !== "stop_pending") break;
    await new Promise((r) => setTimeout(r, 400));
  }
}

// ---- Service migration banner ----
// Shown once, driven by the backend's "suggest-service" event, for auto-updated
// installs still relying on the elevated logon task: offer to adopt the service
// so the app can run unelevated. (Fresh installs get the service from the
// installer, so they never see this.)
async function installServiceFromBanner(): Promise<void> {
  serviceBannerInstall.disabled = true;
  serviceBannerLater.disabled = true;
  serviceBannerText.textContent = "Installing the background service…";
  try {
    // One elevated step (install + start) → a single UAC prompt.
    await invoke("setup_service");
    serviceBanner.classList.add("hidden");
    statusEl.textContent = "Background service installed — restart AllTheThings to search via it.";
  } catch (e) {
    serviceBannerText.textContent = `Could not install the service: ${e}`;
    serviceBannerInstall.disabled = false;
    serviceBannerLater.disabled = false;
  }
}

// ---- Auto-update ----
let pendingUpdate: Update | null = null;

async function checkForUpdates(manual: boolean): Promise<void> {
  if (manual) updateStatus.textContent = "Checking…";
  try {
    const update = await check();
    if (update) {
      pendingUpdate = update;
      updateText.textContent = `AllTheThings ${update.version} is available.`;
      updateBanner.classList.remove("hidden");
      if (manual) updateStatus.textContent = `Update ${update.version} available — see the banner.`;
    } else {
      pendingUpdate = null;
      if (manual) updateStatus.textContent = "You're on the latest version.";
    }
  } catch (e) {
    if (manual) updateStatus.textContent = `Update check failed: ${e}`;
  }
}

async function installUpdate(): Promise<void> {
  if (!pendingUpdate) return;
  updateBanner.classList.add("hidden");
  let total = 0;
  let downloaded = 0;
  try {
    await pendingUpdate.downloadAndInstall((event) => {
      if (event.event === "Started") {
        total = event.data.contentLength ?? 0;
        statusEl.textContent = "Downloading update…";
      } else if (event.event === "Progress") {
        downloaded += event.data.chunkLength;
        statusEl.textContent = total
          ? `Downloading update… ${Math.round((downloaded / total) * 100)}%`
          : "Downloading update…";
      } else if (event.event === "Finished") {
        statusEl.textContent = "Installing update…";
      }
    });
    await relaunch();
  } catch (e) {
    statusEl.textContent = `Update failed: ${e}`;
  }
}

// ---- Export ----
async function exportResults(): Promise<void> {
  if (!hits.length) {
    statusEl.textContent = "Nothing to export";
    return;
  }
  let path: string | null;
  try {
    path = await save({
      defaultPath: "search-results.csv",
      filters: [
        { name: "CSV", extensions: ["csv"] },
        { name: "Text", extensions: ["txt"] },
        { name: "Everything File List", extensions: ["efu"] },
      ],
    });
  } catch (e) {
    reportErr(e);
    return;
  }
  if (!path) return; // cancelled
  // Format follows the chosen extension (only csv/txt/efu are recognised).
  const dot = path.lastIndexOf(".");
  const slash = Math.max(path.lastIndexOf("\\"), path.lastIndexOf("/"));
  const ext = dot > slash ? path.slice(dot + 1).toLowerCase() : "";
  const format = ext === "txt" ? "txt" : ext === "efu" ? "efu" : "csv";
  const unknownExt = ext !== "" && ext !== "csv" && ext !== "txt" && ext !== "efu";
  statusEl.textContent = "Exporting…";
  try {
    const r = await invoke<{ written: number; total: number; capped: boolean }>("export_results", { options, format, path });
    let msg = `Exported ${r.written.toLocaleString()} items`;
    // The effective cap depends on the backend (the in-process index allows more
    // rows than the service), so report the real numbers rather than a fixed cap.
    if (r.written < r.total) msg += ` (of ${r.total.toLocaleString()}; limited to ${r.written.toLocaleString()})`;
    // A content export only scans the first 50,000 filename candidates.
    if (r.capped) msg += " (content scan capped at 50,000 candidates)";
    if (unknownExt) msg += " as CSV";
    statusEl.textContent = `${msg} to ${path}`;
  } catch (e) {
    reportErr(e);
  }
}

// ---- Events ----
q.addEventListener("input", () => {
  options.query = q.value;
  hideHistory();
  scheduleSearch();
});
q.addEventListener("focus", showHistory);
q.addEventListener("blur", () => {
  addHistory(q.value);
  window.setTimeout(hideHistory, 150);
});

viewport.addEventListener(
  "scroll",
  () => {
    hideMenu();
    colMenu.classList.add("hidden");
    // A user scroll cancels an in-progress rename; a scroll we triggered to
    // bring the row into view (startRename) must not.
    if (suppressScrollCancel) suppressScrollCancel = false;
    else cancelRename();
    renderVisible();
  },
  { passive: true },
);
window.addEventListener("resize", renderVisible);

head.addEventListener("contextmenu", (e) => {
  e.preventDefault();
  hideMenu();
  showColumnPicker(e.clientX, e.clientY);
});

optBtn.addEventListener("click", (e) => {
  e.stopPropagation();
  const open = optMenu.classList.contains("hidden");
  hideMenu();
  sizeMenu.classList.add("hidden");
  if (open) {
    const r = optBtn.getBoundingClientRect();
    optMenu.style.left = `${Math.min(r.left, window.innerWidth - 200)}px`;
    optMenu.style.top = `${r.bottom + 4}px`;
    optMenu.classList.remove("hidden");
  } else {
    optMenu.classList.add("hidden");
  }
});

sizeBtn.addEventListener("click", (e) => {
  e.stopPropagation();
  const open = sizeMenu.classList.contains("hidden");
  hideMenu();
  optMenu.classList.add("hidden");
  if (open) {
    const r = sizeBtn.getBoundingClientRect();
    sizeMenu.style.left = `${r.left}px`;
    sizeMenu.style.top = `${r.bottom + 4}px`;
    sizeMenu.classList.remove("hidden");
  } else {
    sizeMenu.classList.add("hidden");
  }
});

rows.addEventListener("dblclick", (e) => {
  const el = (e.target as HTMLElement).closest<HTMLElement>(".row");
  if (!el) return;
  invoke("open_path", { path: hits[Number(el.dataset.i)].path }).catch(reportErr);
});

rows.addEventListener("click", (e) => {
  const el = (e.target as HTMLElement).closest<HTMLElement>(".row");
  if (!el) return;
  selected = Number(el.dataset.i);
  renderVisible();
});

rows.addEventListener("contextmenu", (e) => {
  const el = (e.target as HTMLElement).closest<HTMLElement>(".row");
  if (!el) return;
  e.preventDefault();
  selected = Number(el.dataset.i);
  renderVisible();
  showMenu(e.clientX, e.clientY, hits[selected]);
});

foldersFirstBtn.addEventListener("click", () => {
  options.foldersFirst = !options.foldersFirst;
  try {
    localStorage.setItem(FOLDERS_FIRST_KEY, options.foldersFirst ? "1" : "0");
  } catch {
    /* storage unavailable */
  }
  syncFoldersFirst();
  runSearch();
  q.focus();
});
exportBtn.addEventListener("click", exportResults);
gear.addEventListener("click", openSettings);
settingsClose.addEventListener("click", closeSettings);
brand.addEventListener("click", openAbout);
aboutClose.addEventListener("click", closeAbout);
aboutWebsite.addEventListener("click", (e) => {
  e.preventDefault(); // open in the user's browser, not the app webview
  openUrl(WEBSITE_URL).catch(() => {});
});

// ---- Search builder wiring ----
builderBtn.addEventListener("click", (e) => {
  e.stopPropagation();
  hideMenu();
  sizeMenu.classList.add("hidden");
  optMenu.classList.add("hidden");
  hideHistory();
  openBuilder();
});
bSearch.addEventListener("click", applyBuilder);
bCancel.addEventListener("click", closeBuilder);
bReset.addEventListener("click", resetBuilder);
builderOverlay.addEventListener("click", (e) => {
  if (e.target === builderOverlay) closeBuilder();
});
[bName, bContent, bExt].forEach((el) => {
  el.addEventListener("input", updateBuilderPreview);
  el.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      applyBuilder();
    }
  });
});
bType.addEventListener("change", syncBuilderSize);
[bSize, bDateField, bDateRange, bType].forEach((el) =>
  el.addEventListener("change", updateBuilderPreview),
);
updateInstall.addEventListener("click", installUpdate);
updateLater.addEventListener("click", () => updateBanner.classList.add("hidden"));
serviceBannerInstall.addEventListener("click", installServiceFromBanner);
serviceBannerLater.addEventListener("click", () => serviceBanner.classList.add("hidden"));
checkUpdates.addEventListener("click", () => checkForUpdates(true));
setStartup.addEventListener("change", saveSettings);
setTray.addEventListener("change", saveSettings);
setExplorer.addEventListener("change", saveSettings);
setTheme.addEventListener("change", () => {
  try {
    localStorage.setItem(THEME_KEY, setTheme.value);
  } catch {
    /* storage unavailable */
  }
  applyTheme();
});
window.matchMedia?.("(prefers-color-scheme: dark)")?.addEventListener?.("change", () => {
  if (themePref() === "system") applyTheme();
});
hotkeyInput.addEventListener("focus", () => {
  hotkeyInput.value = "";
  hotkeyInput.placeholder = "Press a key combo…";
  syncHotkeyControls();
});
hotkeyInput.addEventListener("blur", () => {
  hotkeyInput.placeholder = "Click & press keys";
  // Restore the stored value only if nothing was captured (focus cleared the
  // box); a committed capture leaves its new accelerator in place.
  if (!hotkeyInput.value) loadSettings();
});
hotkeyInput.addEventListener("keydown", (e) => {
  e.preventDefault();
  e.stopPropagation();
  if (e.key === "Escape") {
    hotkeyInput.blur();
    return;
  }
  // While only modifiers are held, preview them; commit on a real key.
  if (["Control", "Alt", "Shift", "Meta"].includes(e.key)) {
    const held: string[] = [];
    if (e.ctrlKey) held.push("Ctrl");
    if (e.altKey) held.push("Alt");
    if (e.shiftKey) held.push("Shift");
    hotkeyInput.value = held.length ? held.join("+") + "+…" : "";
    return;
  }
  const accel = accelFromEvent(e);
  if (accel) {
    // Commit, then blur — so the blur handler's restore can't clobber the new value.
    void setHotkey(accel).then(() => hotkeyInput.blur());
  }
});
hotkeyClear.addEventListener("click", () => void setHotkey(""));
svcInstall.addEventListener("click", () => {
  const action = svcInstall.dataset.action;
  if (action === "install") serviceAction("install_service");
  else if (action === "uninstall") serviceAction("uninstall_service");
});
svcPower.addEventListener("click", () => {
  const action = svcPower.dataset.action;
  if (action === "start") serviceAction("start_service");
  else if (action === "stop") serviceAction("stop_service");
});
settingsOverlay.addEventListener("click", (e) => {
  if (e.target === settingsOverlay) closeSettings();
});
aboutOverlay.addEventListener("click", (e) => {
  if (e.target === aboutOverlay) closeAbout();
});
listen("open-settings", openSettings);
listen<string>("shell-error", (e) => reportErr(e.payload));
listen("suggest-service", () => {
  serviceBanner.classList.remove("hidden");
  // Mark seen on actual delivery, so a lost/early event re-offers next launch.
  invoke("mark_service_prompt_seen").catch(() => {});
});
// A running instance receiving an Explorer "Search here" launch.
listen<string>("search-here", (e) => searchHere(e.payload));

// Confirm dialog
confirmOk.addEventListener("click", () => resolveConfirm(true));
confirmCancel.addEventListener("click", () => resolveConfirm(false));
confirmOverlay.addEventListener("click", (e) => {
  if (e.target === confirmOverlay) resolveConfirm(false);
});

// Inline rename input
renameInput.addEventListener("keydown", (e) => {
  e.stopPropagation();
  if (e.key === "Enter") {
    e.preventDefault();
    commitRename(true);
  } else if (e.key === "Escape") {
    e.preventDefault();
    cancelRename();
    q.focus();
  }
});
renameInput.addEventListener("blur", () => commitRename(false));

// Rename / delete shortcuts for the selected row. Suppressed while renaming, a
// modal overlay is open, or a delete confirm is already in flight.
window.addEventListener("keydown", (e) => {
  if (
    renamingIndex >= 0 ||
    selected < 0 ||
    confirmResolve !== null ||
    !settingsOverlay.classList.contains("hidden") ||
    !builderOverlay.classList.contains("hidden") ||
    !aboutOverlay.classList.contains("hidden") ||
    !confirmOverlay.classList.contains("hidden")
  ) {
    return;
  }
  if (e.key === "F2") {
    e.preventDefault();
    startRename();
  } else if (e.key === "Delete" && document.activeElement !== q) {
    e.preventDefault();
    deleteSelected();
  }
});

window.addEventListener("click", (e) => {
  if (!menu.contains(e.target as Node)) hideMenu();
  if (!colMenu.contains(e.target as Node)) colMenu.classList.add("hidden");
  if (e.target !== sizeBtn && !sizeMenu.contains(e.target as Node)) sizeMenu.classList.add("hidden");
  if (e.target !== optBtn && !optMenu.contains(e.target as Node)) optMenu.classList.add("hidden");
  if (e.target !== q && !historyMenu.contains(e.target as Node)) hideHistory();
});
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    hideMenu();
    colMenu.classList.add("hidden");
    sizeMenu.classList.add("hidden");
    optMenu.classList.add("hidden");
    hideHistory();
    closeSettings();
    closeBuilder();
    closeAbout();
    if (!confirmOverlay.classList.contains("hidden")) resolveConfirm(false);
  }
});

q.addEventListener("keydown", (e) => {
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    selected = Math.min(hits.length - 1, Math.max(0, selected + (e.key === "ArrowDown" ? 1 : -1)));
    const top = selected * ROW_HEIGHT;
    if (top < viewport.scrollTop) viewport.scrollTop = top;
    if (top + ROW_HEIGHT > viewport.scrollTop + viewport.clientHeight)
      viewport.scrollTop = top + ROW_HEIGHT - viewport.clientHeight;
    renderVisible();
  } else if (e.key === "Enter" && selected >= 0) {
    invoke("open_path", { path: hits[selected].path }).catch(reportErr);
  }
});

// ---- Init ----
applyTheme();
setCols();
renderHeader();
buildSizeMenu();
buildBuilderControls();
syncControls();
syncFoldersFirst();
q.focus();
// Learn the active backend once so the status bar can show its source.
invoke<boolean>("uses_service")
  .then((v) => {
    backendUsesService = v;
  })
  .catch(() => {});
pollStatus();
// If launched via Explorer "Search here", scope the first search to that folder.
invoke<string | null>("initial_search")
  .then((path) => {
    if (path) searchHere(path);
  })
  .catch(() => {});

// Reveal the window after paint — unless launched into the tray (--minimized).
requestAnimationFrame(() =>
  requestAnimationFrame(async () => {
    const hidden = await invoke<boolean>("start_hidden").catch(() => false);
    if (!hidden) getCurrentWindow().show().catch(() => {});
  }),
);

// Check for updates shortly after launch, without blocking the UI.
window.setTimeout(() => checkForUpdates(false), 4000);
