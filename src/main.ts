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

type SortKey = "name" | "path" | "size" | "modified";
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

type ColKey = "name" | "path" | "size" | "date";
interface Column {
  key: ColKey;
  label: string;
  sort: SortKey;
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

let columns: Column[] = [
  { key: "name", label: "Name", sort: "name", width: 340, flex: false },
  { key: "path", label: "Path", sort: "path", width: 0, flex: true },
  { key: "size", label: "Size", sort: "size", width: 96, flex: false },
  { key: "date", label: "Date modified", sort: "modified", width: 160, flex: false },
];

let hits: Hit[] = [];
let total = 0;
let selected = -1;
let searchSeq = 0;
let debounce: number | undefined;
let highlightTerms: string[] = [];
let resizing = false;
let dragSrc: ColKey | null = null;

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
`;

const q = document.querySelector<HTMLInputElement>("#q")!;
const countEl = document.querySelector<HTMLDivElement>("#count")!;
const viewport = document.querySelector<HTMLDivElement>("#viewport")!;
const spacer = document.querySelector<HTMLDivElement>("#spacer")!;
const rows = document.querySelector<HTMLDivElement>("#rows")!;
const statusEl = document.querySelector<HTMLDivElement>("#status")!;
const head = document.querySelector<HTMLDivElement>("#head")!;
const menu = document.querySelector<HTMLDivElement>("#menu")!;
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
    if (/^(ext|size|file|files|folder|folders|dir):/.test(lower)) continue;
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
  }
}

function renderHeader(): void {
  head.innerHTML = columns
    .map((c) => {
      const ind = c.sort === options.sort ? (options.ascending ? " ▲" : " ▼") : "";
      const grip = c.flex ? "" : `<span class="grip" data-grip="${c.key}"></span>`;
      const align = c.key === "size" ? "text-right pr-2" : "";
      return `<div data-col="${c.key}" draggable="true" class="${align}">${c.label}<span class="ind">${ind}</span>${grip}</div>`;
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
    setTimeout(() => (resizing = false), 0);
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
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

function showMenu(x: number, y: number, h: Hit): void {
  const items: [string, () => void][] = [
    ["Open", () => invoke("open_path", { path: h.path }).catch(reportErr)],
    ["Open containing folder", () => invoke("reveal_path", { path: h.path }).catch(reportErr)],
    ["Copy full path", () => copy(h.path)],
    ["Copy name", () => copy(h.name)],
  ];
  menu.innerHTML = items.map(([label], i) => `<button data-mi="${i}">${esc(label)}</button>`).join("");
  menu.querySelectorAll<HTMLButtonElement>("button").forEach((b, i) => {
    b.onclick = () => {
      hideMenu();
      items[i][1]();
    };
  });
  menu.style.left = `${Math.min(x, window.innerWidth - 220)}px`;
  menu.style.top = `${Math.min(y, window.innerHeight - items.length * 30)}px`;
  menu.classList.remove("hidden");
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
    renderVisible();
  },
  { passive: true },
);
window.addEventListener("resize", renderVisible);

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

window.addEventListener("click", (e) => {
  if (!menu.contains(e.target as Node)) hideMenu();
  if (e.target !== sizeBtn && !sizeMenu.contains(e.target as Node)) sizeMenu.classList.add("hidden");
  if (e.target !== q && !historyMenu.contains(e.target as Node)) hideHistory();
});
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    hideMenu();
    sizeMenu.classList.add("hidden");
    hideHistory();
    closeSettings();
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
