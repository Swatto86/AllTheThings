import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import "./styles.css";

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
}
interface IndexStatus {
  state: "indexing" | "ready" | "error";
  count: number;
  volume: string;
  message: string;
}

type SortKey = "name" | "path" | "size" | "modified" | "created" | "accessed";
interface SearchOptions {
  query: string;
  limit: number;
  matchCase: boolean;
  wholeWord: boolean;
  regex: boolean;
  matchPath: boolean;
  sort: SortKey;
  ascending: boolean;
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

const options: SearchOptions = {
  query: "",
  limit: RESULT_LIMIT,
  matchCase: false,
  wholeWord: false,
  regex: false,
  matchPath: false,
  sort: "name",
  ascending: true,
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
  { key: "ext", label: "Ext", sort: null, width: 70, flex: false },
  { key: "attributes", label: "Attributes", sort: null, width: 96, flex: false },
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
    <div class="flex items-center gap-2 p-2 border-b border-base-300">
      <input id="q" type="text" placeholder="Search all the things…" autocomplete="off" spellcheck="false"
        class="input input-bordered input-sm flex-1 font-mono" />
      <div class="relative">
        <button id="sizebtn" title="Filter by size" class="btn btn-xs">Size ▾</button>
        <div id="sizemenu" class="menu-pop hidden"></div>
      </div>
      <div class="join">
        <button data-opt="matchCase" title="Match case" class="opt btn btn-xs join-item">Aa</button>
        <button data-opt="wholeWord" title="Match whole word" class="opt btn btn-xs join-item">W</button>
        <button data-opt="regex" title="Use regular expression" class="opt btn btn-xs join-item">.*</button>
        <button data-opt="matchPath" title="Match full path" class="opt btn btn-xs join-item">/</button>
      </div>
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
        <span>Start with Windows<br><span class="hint">Runs elevated at sign-in, indexes in the background, lives in the tray</span></span>
        <input type="checkbox" id="set-startup" class="toggle toggle-sm toggle-primary" />
      </label>
      <label class="settings-row">
        <span>Close button minimizes to tray<br><span class="hint">Otherwise the window closing quits the app</span></span>
        <input type="checkbox" id="set-tray" class="toggle toggle-sm toggle-primary" />
      </label>
      <div class="settings-row">
        <span>Updates<br><span class="hint" id="update-status">AllTheThings checks for updates on launch.</span></span>
        <button id="check-updates" class="btn btn-sm">Check now</button>
      </div>
      <div id="settings-msg" class="settings-msg"></div>
      <div class="settings-actions"><button id="settings-close" class="btn btn-sm">Close</button></div>
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
const historyMenu = document.querySelector<HTMLDivElement>("#history-menu")!;
const gear = document.querySelector<HTMLButtonElement>("#gear")!;
const settingsOverlay = document.querySelector<HTMLDivElement>("#settings-overlay")!;
const setStartup = document.querySelector<HTMLInputElement>("#set-startup")!;
const setTray = document.querySelector<HTMLInputElement>("#set-tray")!;
const settingsMsg = document.querySelector<HTMLDivElement>("#settings-msg")!;
const settingsClose = document.querySelector<HTMLButtonElement>("#settings-close")!;
const updateBanner = document.querySelector<HTMLDivElement>("#update-banner")!;
const updateText = document.querySelector<HTMLSpanElement>("#update-text")!;
const updateInstall = document.querySelector<HTMLButtonElement>("#update-install")!;
const updateLater = document.querySelector<HTMLButtonElement>("#update-later")!;
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

// Plain terms to highlight: quoted phrases and bare words, minus operators,
// negated terms, ext:/size:/file:/folder: functions, and wildcards.
function computeHighlightTerms(query: string, regex: boolean): string[] {
  if (regex) return [];
  const terms: string[] = [];
  const re = /"([^"]*)"|(\S+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(query)) !== null) {
    if (m[1] !== undefined) {
      if (m[1]) terms.push(m[1].toLowerCase());
      continue;
    }
    let tok = m[2];
    if (tok === "|" || tok.startsWith("!")) continue;
    const lower = tok.toLowerCase();
    if (/^(ext|size|file|files|folder|folders|dir|dm|dc|da|attrib):/.test(lower)) continue;
    if (lower.startsWith("path:")) tok = tok.slice(5);
    if (!tok || tok.includes("*") || tok.includes("?")) continue;
    terms.push(tok.toLowerCase());
  }
  return terms;
}

// Escape `raw` and wrap any matched term occurrences in <mark>.
function highlight(raw: string): string {
  if (!highlightTerms.length) return esc(raw);
  const lower = raw.toLowerCase();
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
  highlightTerms = computeHighlightTerms(options.query, options.regex);
  try {
    const res = await invoke<SearchResponse>("search", { options });
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
    countEl.textContent = `${total.toLocaleString()} found · ${res.tookMs} ms`;
    renderVisible();
  } catch (e) {
    statusEl.textContent = `Search error: ${e}`;
  }
}

function scheduleSearch(): void {
  window.clearTimeout(debounce);
  debounce = window.setTimeout(runSearch, 60);
}

async function pollStatus(): Promise<void> {
  try {
    const s = await invoke<IndexStatus>("index_status");
    statusEl.textContent =
      s.state === "indexing"
        ? `Indexing ${s.volume}… ${s.count.toLocaleString()} items`
        : s.state === "error"
          ? `Index error: ${s.message}`
          : `Ready · ${s.count.toLocaleString()} items on ${s.volume}`;
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
}

function cancelRename(): void {
  if (renamingIndex < 0) return;
  renamingIndex = -1;
  renameInput.classList.add("hidden");
}

async function commitRename(restoreFocus: boolean): Promise<void> {
  if (renamingIndex < 0) return;
  const idx = renamingIndex;
  const h = hits[idx];
  const newName = renameInput.value.trim();
  cancelRename();
  // On Enter, return focus to the search box so keyboard nav keeps working; on
  // blur, leave focus wherever the user clicked.
  if (restoreFocus) q.focus();
  if (!newName || newName === h.name) return;
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
  if (!(await confirmDelete(h.name))) return;
  try {
    await invoke("delete_path", { path: h.path });
    hits.splice(selected, 1);
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
  historyMenu.innerHTML = history.map((h, i) => `<button data-hi="${i}">${esc(h)}</button>`).join("");
  historyMenu.querySelectorAll<HTMLButtonElement>("button").forEach((b, i) => {
    b.addEventListener("mousedown", (e) => e.preventDefault()); // keep input focus
    b.addEventListener("click", () => {
      q.value = history[i];
      options.query = history[i];
      hideHistory();
      runSearch();
      q.focus();
    });
  });
  const r = q.getBoundingClientRect();
  historyMenu.style.left = `${r.left}px`;
  historyMenu.style.top = `${r.bottom + 2}px`;
  historyMenu.style.minWidth = `${r.width}px`;
  historyMenu.classList.remove("hidden");
}

function hideHistory(): void {
  historyMenu.classList.add("hidden");
}

function syncControls(): void {
  document.querySelectorAll<HTMLButtonElement>(".opt").forEach((b) => {
    const on = options[b.dataset.opt as "matchCase" | "wholeWord" | "regex" | "matchPath"];
    b.classList.toggle("btn-primary", on);
  });
}

// ---- Settings ----
interface Settings {
  closeToTray: boolean;
  runAtStartup: boolean;
}

async function loadSettings(): Promise<void> {
  try {
    const s = await invoke<Settings>("get_settings");
    setStartup.checked = s.runAtStartup;
    setTray.checked = s.closeToTray;
    settingsMsg.textContent = "";
  } catch (e) {
    settingsMsg.textContent = `Could not load settings: ${e}`;
  }
}

async function saveSettings(): Promise<void> {
  settingsMsg.textContent = "";
  try {
    await invoke("set_settings", {
      settings: { runAtStartup: setStartup.checked, closeToTray: setTray.checked },
    });
  } catch (e) {
    settingsMsg.textContent = `${e}`;
    await loadSettings(); // re-sync toggles with reality (e.g. task creation failed)
  }
}

function openSettings(): void {
  loadSettings();
  settingsOverlay.classList.remove("hidden");
}

function closeSettings(): void {
  settingsOverlay.classList.add("hidden");
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

document.querySelectorAll<HTMLButtonElement>(".opt").forEach((b) => {
  b.addEventListener("click", () => {
    const key = b.dataset.opt as "matchCase" | "wholeWord" | "regex" | "matchPath";
    options[key] = !options[key];
    syncControls();
    runSearch();
    q.focus();
  });
});

sizeBtn.addEventListener("click", (e) => {
  e.stopPropagation();
  const open = sizeMenu.classList.contains("hidden");
  hideMenu();
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

gear.addEventListener("click", openSettings);
settingsClose.addEventListener("click", closeSettings);
updateInstall.addEventListener("click", installUpdate);
updateLater.addEventListener("click", () => updateBanner.classList.add("hidden"));
checkUpdates.addEventListener("click", () => checkForUpdates(true));
setStartup.addEventListener("change", saveSettings);
setTray.addEventListener("change", saveSettings);
settingsOverlay.addEventListener("click", (e) => {
  if (e.target === settingsOverlay) closeSettings();
});
listen("open-settings", openSettings);
listen<string>("shell-error", (e) => reportErr(e.payload));

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
  if (e.target !== q && !historyMenu.contains(e.target as Node)) hideHistory();
});
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    hideMenu();
    colMenu.classList.add("hidden");
    sizeMenu.classList.add("hidden");
    hideHistory();
    closeSettings();
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
setCols();
renderHeader();
buildSizeMenu();
syncControls();
q.focus();
pollStatus();

// Reveal the window after paint — unless launched into the tray (--minimized).
requestAnimationFrame(() =>
  requestAnimationFrame(async () => {
    const hidden = await invoke<boolean>("start_hidden").catch(() => false);
    if (!hidden) getCurrentWindow().show().catch(() => {});
  }),
);

// Check for updates shortly after launch, without blocking the UI.
window.setTimeout(() => checkForUpdates(false), 4000);
