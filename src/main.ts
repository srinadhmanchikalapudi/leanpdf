import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import "./style.css";

type Settings = { folder: string | null; last_file: string | null; last_page: number | null };
type DocInfo = { id: number; page_count: number; pages: [number, number][] };
type Rect = [number, number, number, number]; // left, bottom, right, top (PDF points)
type Annot = {
  page: number; index: number; kind: string; author: string | null; contents: string | null; mono?: boolean;
  modified: string | null; text: string | null; rect: Rect;
};
type FileEntry = { path: string; rel: string; status: string; pages: number; note: string | null };
type TreeNode = { name: string; path: string; folders: Map<string, TreeNode>; files: FileEntry[]; count: number };
type Hit = { page: number; snippet: string };
type FileHits = { path: string; rel: string; name_match: boolean; hits: Hit[] };
type Selection = { rects: Rect[]; text: string };
type Summary = { total: number; pending: number; no_text: number; errors: number };
type PageView = {
  el: HTMLDivElement; layer: HTMLDivElement; img: HTMLImageElement | null;
  key: string | null; loading: string | null;
};

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const app = $("app"), results = $("results"), statusEl = $("status"), q = $<HTMLInputElement>("q");
const scroller = $("scroller"), pagesEl = $("pages"), emptyEl = $("empty");
const pageInput = $<HTMLInputElement>("page-input"), pageTotal = $("page-total");
const zoomLabel = $("zoom-label"), docTitle = $("doc-title"), commentsEl = $("comments");
const commentList = $("comment-list"), commentCount = $("comment-count");

const PT = 96 / 72; // CSS pixels per PDF point at 100%
const MIN_ZOOM = 0.25, MAX_ZOOM = 5, MAX_INFLIGHT = 3;
const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });
const pageBase = convertFileSrc("", "pdfpage").replace(/\/+$/, "");

let settings: Settings = { folder: null, last_file: null, last_page: null };
let files: FileEntry[] = [];
let tree: TreeNode = newNode("", "");
let orderedFiles: FileEntry[] = []; // files in the order the tree shows them
let scope = ""; // folder (relative path) that search is limited to; "" = everything
const expanded = new Set<string>();
let doc: DocInfo | null = null;
let docPath: string | null = null;
let pages: PageView[] = [];
let zoom = 1;
let fitWidth = true;
let openToken = 0;
let inflight = 0;
let currentPage = 0;
let annots: Annot[] = [];
let searchSeq = 0;
let lastRescan = Date.now();
let indexing = false;
const wanted = new Set<number>();

/* ---------- small helpers ---------- */

const esc = (s: string) =>
  s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
const baseName = (rel: string) => rel.slice(rel.lastIndexOf("/") + 1);
const dirName = (rel: string) => (rel.includes("/") ? rel.slice(0, rel.lastIndexOf("/")) : "");
const errText = (e: unknown) => (typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e));

let toastTimer = 0;
function toast(msg: string) {
  const t = $("toast");
  t.textContent = msg;
  t.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => t.classList.remove("show"), 5000);
}

function debounce<A extends unknown[]>(fn: (...a: A) => void, ms: number) {
  let t = 0;
  return (...a: A) => {
    clearTimeout(t);
    t = window.setTimeout(() => fn(...a), ms);
  };
}

function termsFromQuery(text: string): string[] {
  const t = text.trim();
  if (t.length >= 2 && t.startsWith('"') && t.endsWith('"')) return [t.slice(1, -1).trim()];
  return t.split(/[^\p{L}\p{N}]+/u).filter((w) => w.length >= 2);
}

function formatPdfDate(raw: string | null): string {
  if (!raw) return "";
  const m = /^D?:?(\d{4})(\d{2})?(\d{2})?(\d{2})?(\d{2})?/.exec(raw);
  if (!m) return raw;
  const d = new Date(+m[1], +(m[2] ?? 1) - 1, +(m[3] ?? 1), +(m[4] ?? 0), +(m[5] ?? 0));
  return d.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

/* ---------- folder list + search ---------- */

function newNode(name: string, path: string): TreeNode {
  return { name, path, folders: new Map(), files: [], count: 0 };
}

function buildTree() {
  tree = newNode("", "");
  for (const f of files) {
    const parts = f.rel.split("/");
    let node = tree;
    node.count++;
    for (let i = 0; i < parts.length - 1; i++) {
      let child = node.folders.get(parts[i]);
      if (!child) {
        child = newNode(parts[i], parts.slice(0, i + 1).join("/"));
        node.folders.set(parts[i], child);
      }
      child.count++;
      node = child;
    }
    node.files.push(f);
  }
  // Folders first, then files, each in natural order. Prev/Next follow this order.
  orderedFiles = [];
  const walk = (n: TreeNode) => {
    for (const c of sortedFolders(n)) walk(c);
    for (const f of sortedFiles(n)) orderedFiles.push(f);
  };
  walk(tree);
}
const sortedFolders = (n: TreeNode) => [...n.folders.values()].sort((a, b) => collator.compare(a.name, b.name));
const sortedFiles = (n: TreeNode) => [...n.files].sort((a, b) => collator.compare(baseName(a.rel), baseName(b.rel)));

function saveTreeState() {
  try {
    localStorage.setItem("tree", JSON.stringify({ folder: settings.folder, scope, expanded: [...expanded] }));
  } catch { /* storage unavailable */ }
}
function loadTreeState() {
  try {
    const s = JSON.parse(localStorage.getItem("tree") ?? "null");
    if (s && s.folder === settings.folder) {
      scope = s.scope ?? "";
      (s.expanded as string[]).forEach((p) => expanded.add(p));
    }
  } catch { /* ignore */ }
}

function updateScopeUi() {
  const name = scope ? baseName(scope) : settings.folder ? $("folder-name").textContent : "";
  q.placeholder = scope ? `Search in “${name}”   (Ctrl+K)` : "Search inside all PDFs   (Ctrl+K)";
}

async function refreshFiles() {
  if (!settings.folder) return;
  files = await invoke<FileEntry[]>("list_files");
  buildTree();
  if (scope && !files.some((f) => f.rel.startsWith(scope + "/"))) scope = "";
  updateScopeUi();
  updateNav();
  if (!q.value.trim()) renderFileList();
}

/** Opens every folder above the current file so it is visible in the tree. */
function revealCurrent() {
  const f = files.find((x) => x.path === docPath);
  if (!f) return;
  const parts = f.rel.split("/");
  for (let i = 1; i < parts.length; i++) expanded.add(parts.slice(0, i).join("/"));
  saveTreeState();
}

function renderEmptyFolder() {
  results.innerHTML = `<div class="pick">Pick the folder that holds your PDFs.<br>It is remembered, and everything inside it becomes searchable.<br><button id="pick-folder">Choose folder…</button></div>`;
  $("pick-folder").onclick = chooseFolder;
}

function fileButton(f: { path: string; rel: string }, extra = ""): string {
  const active = f.path === docPath ? " active" : "";
  const dir = dirName(f.rel);
  return `<button class="file${active}" data-path="${esc(f.path)}" title="${esc(f.path)}">
    <span class="name">${esc(baseName(f.rel))}${extra}</span>${dir ? `<span class="dir">${esc(dir)}</span>` : ""}</button>`;
}

function statusTag(f: FileEntry): string {
  if (f.status === "notext") return `<span class="tag" title="No text layer, so it can't be searched (scanned image)">scan</span>`;
  if (f.status === "error")
    return `<span class="tag bad" title="${esc(f.note ?? "Could not be read")}. Press ↻ to retry.">unreadable</span>`;
  return "";
}

function renderFileList() {
  if (!settings.folder) return renderEmptyFolder();
  if (!files.length) {
    results.innerHTML = `<div class="nothing">${indexing ? "Scanning folder…" : "No PDFs found in this folder."}</div>`;
    return;
  }
  const rootName = $("folder-name").textContent ?? "";
  const row = (cls: string, depth: number, attrs: string, inner: string) =>
    `<div class="row ${cls}" style="--d:${depth}" ${attrs}>${inner}</div>`;
  let html = row(
    `folder root${scope === "" ? " sel" : ""}`,
    0,
    `data-folder=""`,
    `<span class="chev"></span><span class="fname">${esc(rootName)}</span><span class="cnt">${tree.count}</span>`,
  );
  const emit = (node: TreeNode, depth: number) => {
    for (const c of sortedFolders(node)) {
      const open = expanded.has(c.path);
      html += row(
        `folder${scope === c.path ? " sel" : ""}`,
        depth,
        `data-folder="${esc(c.path)}" title="${esc(c.path)}"`,
        `<span class="chev${open ? " open" : ""}" data-chev="1">›</span><span class="fname">${esc(c.name)}</span><span class="cnt">${c.count}</span>`,
      );
      if (open) emit(c, depth + 1);
    }
    for (const f of sortedFiles(node)) {
      const active = f.path === docPath ? " active" : "";
      html += `<button class="row file${active}" style="--d:${depth}" data-path="${esc(f.path)}" title="${esc(f.rel)}"><span class="name">${esc(baseName(f.rel))}${statusTag(f)}</span></button>`;
    }
  };
  emit(tree, 1);
  results.innerHTML = html;
  results.querySelector(".file.active")?.scrollIntoView({ block: "nearest" });
}

results.addEventListener("click", (ev) => {
  const row = (ev.target as HTMLElement).closest<HTMLElement>(".row.folder");
  if (!row) return;
  const path = row.dataset.folder ?? "";
  const onChevron = !!(ev.target as HTMLElement).closest("[data-chev]");
  if (path) {
    if (onChevron || (scope === path && expanded.has(path))) {
      if (expanded.has(path)) expanded.delete(path);
      else expanded.add(path);
    } else expanded.add(path);
  }
  if (!onChevron) scope = path;
  saveTreeState();
  updateScopeUi();
  if (q.value.trim()) void runSearch();
  else renderFileList();
});

function snippetHtml(s: string) {
  return esc(s).replace(/\u0001/g, "<mark>").replace(/\u0002/g, "</mark>");
}

function renderResults(list: FileHits[], query: string) {
  if (!list.length) {
    results.innerHTML = `<div class="nothing">No matches for “${esc(query)}”.${indexing ? "<br>Indexing is still running, so results may be incomplete." : ""}</div>`;
    return;
  }
  const total = list.reduce((n, f) => n + f.hits.length, 0);
  let html = `<div class="nothing" style="padding:4px 12px 8px">${list.length} file${list.length === 1 ? "" : "s"}, ${total} page match${total === 1 ? "" : "es"}</div>`;
  list.forEach((f, fi) => {
    const shown = f.hits.slice(0, 4);
    const hits = (arr: Hit[]) =>
      arr.map((h) => `<button class="hit" data-fi="${fi}" data-page="${h.page}"><span class="pg">p.${h.page}</span><span class="snip">${snippetHtml(h.snippet)}</span></button>`).join("");
    html += `<div class="group" data-fi="${fi}">${fileButton(f, f.name_match ? `<span class="tag">name</span>` : "")}${hits(shown)}`;
    if (f.hits.length > shown.length)
      html += `<button class="more" data-fi="${fi}">+ ${f.hits.length - shown.length} more pages</button>`;
    html += `</div>`;
  });
  results.innerHTML = html;
  results.scrollTop = 0;
  lastResults = list;
}

let lastResults: FileHits[] = [];

async function runSearch() {
  const text = q.value.trim();
  const seq = ++searchSeq;
  if (!text) return renderFileList();
  if (!settings.folder) return renderEmptyFolder();
  try {
    const list = await invoke<FileHits[]>("search", { query: text, scope });
    list.forEach((f) => f.hits.sort((a, b) => a.page - b.page)); // read in page order
    if (seq === searchSeq) renderResults(list, text);
  } catch (e) {
    if (seq === searchSeq) results.innerHTML = `<div class="nothing">${esc(errText(e))}</div>`;
  }
}
const runSearchSoon = debounce(runSearch, 180);

results.addEventListener("click", (ev) => {
  const t = ev.target as HTMLElement;
  const more = t.closest<HTMLElement>(".more");
  if (more) {
    const f = lastResults[+more.dataset.fi!];
    more.outerHTML = f.hits
      .slice(4)
      .map((h) => `<button class="hit" data-fi="${more.dataset.fi}" data-page="${h.page}"><span class="pg">p.${h.page}</span><span class="snip">${snippetHtml(h.snippet)}</span></button>`)
      .join("");
    return;
  }
  const hit = t.closest<HTMLElement>(".hit");
  if (hit) {
    const f = lastResults[+hit.dataset.fi!];
    void openFile(f.path, +hit.dataset.page! - 1, termsFromQuery(q.value), f.hits.map((h) => h.page - 1));
    return;
  }
  const file = t.closest<HTMLElement>(".file");
  if (file) {
    const path = file.dataset.path!;
    const inResults = q.value.trim() ? lastResults.find((f) => f.path === path) : undefined;
    const first = inResults?.hits[0];
    void openFile(path, first ? first.page - 1 : 0, first ? termsFromQuery(q.value) : [], inResults?.hits.map((h) => h.page - 1) ?? []);
  }
});

function setActiveFile() {
  revealCurrent();
  if (!q.value.trim()) renderFileList();
  results.querySelectorAll(".file.active").forEach((e) => e.classList.remove("active"));
  results.querySelectorAll<HTMLElement>(".file").forEach((e) => {
    if (e.dataset.path === docPath) e.classList.add("active");
  });
}

/* ---------- folder + indexing status ---------- */

function showFolder() {
  const name = settings.folder ? settings.folder.split(/[\\/]/).filter(Boolean).pop()! : "Choose a folder…";
  $("folder-name").textContent = name;
  $("choose-folder").title = settings.folder ?? "Choose the folder to search";
}

async function chooseFolder() {
  const picked = await open({ directory: true, multiple: false, defaultPath: settings.folder ?? undefined });
  if (typeof picked !== "string") return;
  settings.folder = picked;
  files = [];
  scope = "";
  expanded.clear();
  buildTree();
  saveTreeState();
  indexing = true;
  showFolder();
  updateScopeUi();
  statusEl.textContent = "Scanning folder…";
  renderFileList();
  await invoke("set_folder", { path: picked });
  q.focus();
}

async function showSummary() {
  if (!settings.folder) return (statusEl.textContent = "");
  try {
    const s = await invoke<Summary>("index_summary");
    const parts = [`${s.total} PDF${s.total === 1 ? "" : "s"} indexed`];
    if (s.no_text) parts.push(`${s.no_text} scanned (no text)`);
    if (s.errors) parts.push(`${s.errors} unreadable`);
    statusEl.textContent = parts.join(" · ");
  } catch {
    statusEl.textContent = "";
  }
}

void listen<null>("index-files", () => {
  indexing = true;
  void refreshFiles();
});
void listen<{ done: number; total: number; current: string }>("index-progress", (e) => {
  indexing = true;
  const { done, total, current } = e.payload;
  statusEl.innerHTML = `Indexing ${done + 1} / ${total} · ${esc(baseName(current))}<div class="bar"><i style="width:${(done / total) * 100}%"></i></div>`;
});
void listen<Summary>("index-done", () => {
  indexing = false;
  void showSummary();
  void refreshFiles();
  if (q.value.trim()) void runSearch();
});
void listen<string>("index-error", (e) => {
  indexing = false;
  statusEl.textContent = e.payload;
});

/* ---------- viewer: layout ---------- */

const cssWidth = (i: number) => doc!.pages[i][0] * PT * zoom;
const cssHeight = (i: number) => doc!.pages[i][1] * PT * zoom;
const keyFor = (i: number) => `${doc!.id}:${wantedPixels(i)}`;
function wantedPixels(i: number) {
  return Math.min(4000, Math.max(100, Math.round(cssWidth(i) * (window.devicePixelRatio || 1))));
}
let renderVer = 0; // bumped when a page's content changes (new annotation)
const pageUrl = (i: number) => `${pageBase}/${doc!.id}/${i}/${wantedPixels(i)}?v=${renderVer}`;

const observer = new IntersectionObserver(
  (entries) => {
    for (const en of entries) {
      const i = +(en.target as HTMLElement).dataset.i!;
      if (en.isIntersecting) wanted.add(i);
      else {
        wanted.delete(i);
        unloadPage(i);
      }
    }
    pump();
  },
  { root: scroller, rootMargin: "1100px 0px" },
);

function unloadPage(i: number) {
  const p = pages[i];
  if (!p) return;
  p.img?.remove();
  p.img = null;
  p.key = null;
  p.loading = null;
}

function applySizes() {
  if (!doc) return;
  pages.forEach((p, i) => {
    p.el.style.width = `${cssWidth(i)}px`;
    p.el.style.height = `${cssHeight(i)}px`;
  });
  zoomLabel.textContent = `${Math.round(zoom * 100)}%`;
}

function computeFit(): number {
  if (!doc) return 1;
  const avail = scroller.clientWidth - 40;
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, avail / (doc.pages[Math.min(currentPage, doc.pages.length - 1)][0] * PT)));
}

function setZoom(z: number, isFit = false) {
  if (!doc) return;
  fitWidth = isFit;
  z = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, z));
  const p = pages[currentPage];
  const frac = p ? (scroller.scrollTop - p.el.offsetTop) / Math.max(1, p.el.offsetHeight) : 0;
  zoom = z;
  applySizes();
  if (p) scroller.scrollTop = p.el.offsetTop + frac * p.el.offsetHeight;
  reloadSoon();
}
const reloadSoon = debounce(() => pump(), 160);

/* ---------- viewer: page loading ---------- */

function pump() {
  if (!doc) return;
  while (inflight < MAX_INFLIGHT) {
    let best = -1, bestDist = Infinity;
    for (const i of wanted) {
      const p = pages[i];
      if (!p || p.loading || p.key === keyFor(i)) continue;
      const d = Math.abs(i - currentPage);
      if (d < bestDist) { best = i; bestDist = d; }
    }
    if (best < 0) return;
    startLoad(best);
  }
}

function startLoad(i: number) {
  const p = pages[i];
  const key = keyFor(i);
  const token = openToken;
  p.loading = key;
  inflight++;
  const im = new Image();
  im.className = "pg";
  im.alt = "";
  im.decoding = "async";
  im.draggable = false;
  im.onload = () => {
    inflight--;
    if (token === openToken && p.loading === key && wanted.has(i)) {
      if (p.img) p.img.replaceWith(im);
      else p.el.prepend(im);
      p.img = im;
      p.key = key;
    }
    if (p.loading === key) p.loading = null;
    pump();
  };
  im.onerror = () => {
    inflight--;
    if (token === openToken && p.loading === key) {
      p.loading = null;
      p.key = key; // don't retry in a loop; a zoom change or reopen will
    }
    pump();
  };
  im.src = pageUrl(i);
}

/* ---------- viewer: opening, navigation ---------- */

function showMessage(text: string) {
  emptyEl.textContent = text;
}

function buildPages(info: DocInfo) {
  observer.disconnect();
  wanted.clear();
  pages = [];
  inflight = 0;
  const frag = document.createDocumentFragment();
  info.pages.forEach((_, i) => {
    const el = document.createElement("div");
    el.className = "page";
    el.dataset.i = String(i);
    const layer = document.createElement("div");
    layer.className = "layer";
    el.appendChild(layer);
    frag.appendChild(el);
    pages.push({ el, layer, img: null, key: null, loading: null });
  });
  pagesEl.replaceChildren(frag);
  doc = info;
  currentPage = 0;
  zoom = computeFit();
  fitWidth = true;
  applySizes();
  pages.forEach((p) => observer.observe(p.el));
}

type OpenResult = "ok" | "fail" | "stale";

/** `quiet` suppresses the error toast (used while skipping unreadable files). */
async function openFile(path: string, page = 0, terms: string[] = [], hitPages: number[] = [], quiet = false): Promise<OpenResult> {
  if (doc && docPath === path) {
    goToPage(page);
    void highlight(terms, hitPages.length ? hitPages : [page]);
    scroller.focus({ preventScroll: true });
    return "ok";
  }
  await saveNow(); // never leave a document with unsaved comments
  clearSelection();
  const token = ++openToken;
  showMessage("");
  const entry = files.find((f) => f.path === path);
  try {
    const info = await invoke<DocInfo>("open_pdf", { path });
    if (token !== openToken) return "stale";
    if (entry && entry.status === "error") {
      entry.status = "done"; // it opens now, so don't keep calling it unreadable
      entry.note = null;
    }
    docPath = path;
    annots = [];
    renderComments();
    buildPages(info);
    docTitle.textContent = baseName(path.replace(/\\/g, "/"));
    docTitle.title = path;
    pageTotal.textContent = `/ ${info.page_count}`;
    document.title = `${docTitle.textContent} – LeanPDF`;
    updateNav();
    setActiveFile();
    goToPage(Math.min(page, info.page_count - 1));
    scroller.focus({ preventScroll: true });
    void highlight(terms, hitPages.length ? hitPages : [page]);
    window.setTimeout(() => void loadComments(info, token), 350);
    void invoke("remember_last", { path, page: Math.min(page, info.page_count - 1) });
    return "ok";
  } catch (e) {
    if (token !== openToken) return "stale";
    const msg = errText(e);
    if (entry) {
      entry.status = "error";
      entry.note = msg;
      if (!q.value.trim()) renderFileList();
    }
    if (!quiet) toast(`${baseName(path.replace(/\\/g, "/"))}: ${msg}`);
    if (!doc) showMessage(msg);
    return "fail";
  }
}

function goToPage(i: number, rect?: Rect) {
  const p = pages[i];
  if (!p || !doc) return;
  let y = p.el.offsetTop - 10;
  if (rect) {
    const ph = doc.pages[i][1];
    y = p.el.offsetTop + ((ph - rect[3]) / ph) * p.el.offsetHeight - scroller.clientHeight * 0.3;
  }
  scroller.scrollTop = Math.max(0, y);
  currentPage = i;
  pageInput.value = String(i + 1);
  pump();
}

function updateCurrentPage() {
  if (!pages.length) return;
  const y = scroller.scrollTop + scroller.clientHeight * 0.3;
  let i = Math.min(currentPage, pages.length - 1);
  while (i > 0 && pages[i].el.offsetTop > y) i--;
  while (i < pages.length - 1 && pages[i + 1].el.offsetTop <= y) i++;
  if (i !== currentPage) {
    currentPage = i;
    pump();
  }
  if (document.activeElement !== pageInput) pageInput.value = String(i + 1);
  rememberSoon();
}
let scrollQueued = false;
scroller.addEventListener("scroll", () => {
  if (scrollQueued) return;
  scrollQueued = true;
  requestAnimationFrame(() => {
    scrollQueued = false;
    updateCurrentPage();
  });
});
const rememberSoon = debounce(() => {
  if (docPath) void invoke("remember_last", { path: docPath, page: currentPage });
}, 900);

function addRect(layer: HTMLElement, r: Rect, size: [number, number], cls: string) {
  const [l, b, rt, t] = r, [pw, ph] = size;
  const d = document.createElement("div");
  d.className = cls;
  d.style.left = `${(l / pw) * 100}%`;
  d.style.top = `${((ph - t) / ph) * 100}%`;
  d.style.width = `${((rt - l) / pw) * 100}%`;
  d.style.height = `${((t - b) / ph) * 100}%`;
  layer.appendChild(d);
}

async function highlight(terms: string[], pageIdx: number[]) {
  pages.forEach((p) => p.layer.querySelectorAll(".found").forEach((e) => e.remove()));
  if (!doc || !terms.length) return;
  const token = openToken, id = doc.id;
  for (const i of [...new Set(pageIdx)].slice(0, 25)) {
    try {
      const rects = await invoke<Rect[]>("find_text", { doc: id, page: i, terms });
      if (token !== openToken || !pages[i]) return;
      rects.forEach((r) => addRect(pages[i].layer, r, doc!.pages[i], "found"));
    } catch {
      return;
    }
  }
}

/* ---------- comments ---------- */

async function loadComments(info: DocInfo, token: number) {
  const step = 20;
  for (let from = 0; from < info.page_count; from += step) {
    if (token !== openToken) return;
    try {
      const batch = await invoke<Annot[]>("annotations", { doc: info.id, from, to: from + step });
      if (token !== openToken) return;
      if (batch.length) {
        annots.push(...batch);
        renderComments();
      }
    } catch {
      return;
    }
  }
}

function renderComments() {
  commentCount.textContent = String(annots.length);
  if (!annots.length) {
    commentList.innerHTML = `<div class="nothing">No comments or markup in this PDF.</div>`;
    return;
  }
  commentList.innerHTML = annots
    .map((a, i) => {
      const meta = [`<span class="kind">${esc(a.kind)}</span>`, `p.${a.page + 1}`, a.author ? esc(a.author) : "", esc(formatPdfDate(a.modified))]
        .filter(Boolean)
        .map((m) => `<span>${m}</span>`)
        .join("");
      const editable = !["text box", "stamp", "attachment", "caret"].includes(a.kind);
      const acts = `<div class="acts">${editable ? `<button data-act="edit">${a.contents ? "Edit" : "Add note"}</button>` : ""}<button class="del" data-act="del">Delete</button></div>`;
      return `<div class="c" data-i="${i}"><div class="meta">${meta}</div>${a.contents ? `<div class="body${a.mono || /	|^ {2,}|\S {2,}\S/m.test(a.contents) ? " mono" : ""}">${esc(a.contents)}</div>` : ""}${a.text ? `<div class="quote">${esc(a.text)}</div>` : ""}${acts}</div>`;
    })
    .join("");
}

commentList.addEventListener("click", (ev) => {
  const t = ev.target as HTMLElement;
  const c = t.closest<HTMLElement>(".c");
  if (!c || !doc) return;
  if (t.closest("button, textarea")) return; // edit/delete handled in commentActions
  const a = annots[+c.dataset.i!];
  goToPage(a.page, a.rect);
  const layer = pages[a.page].layer;
  layer.querySelectorAll(".flash").forEach((e) => e.remove());
  addRect(layer, a.rect, doc.pages[a.page], "flash");
  window.setTimeout(() => layer.querySelectorAll(".flash").forEach((e) => e.remove()), 2700);
});

/* ---------- editing: selection, markup, notes, saving ---------- */

const COLORS: [number, number, number][] = [[255, 225, 77], [123, 224, 107], [107, 196, 255], [255, 143, 184]];
const seltool = $("seltool"), noteEditor = $("noteeditor"), noteText = $<HTMLTextAreaElement>("note-text");
const saveState = $("save-state");
let sel: { page: number; rects: Rect[]; text: string } | null = null;
let drag: { page: number; start: [number, number]; last: [number, number] | null; busy: boolean; moved: boolean } | null = null;
let noteTool = false;
let dirty = false;
let saveTimer = 0;
let saveChain: Promise<void> = Promise.resolve();
type NoteCtx =
  | { mode: "sticky"; page: number; x: number; y: number }
  | { mode: "highlight"; page: number; rects: Rect[] }
  | { mode: "edit"; annot: Annot };
let noteCtx: NoteCtx | null = null;

function toPdf(i: number, e: { clientX: number; clientY: number }): [number, number] {
  const r = pages[i].el.getBoundingClientRect();
  const [pw, ph] = doc!.pages[i];
  return [((e.clientX - r.left) / r.width) * pw, ph - ((e.clientY - r.top) / r.height) * ph];
}

function clearSelection() {
  if (sel) pages[sel.page]?.layer.querySelectorAll(".sel").forEach((e) => e.remove());
  sel = null;
  seltool.hidden = true;
}

function drawSelection() {
  if (!sel || !doc) return;
  const layer = pages[sel.page].layer;
  layer.querySelectorAll(".sel").forEach((e) => e.remove());
  sel.rects.forEach((r) => addRect(layer, r, doc!.pages[sel!.page], "sel"));
}

async function updateSelection() {
  const d = drag;
  if (!d || d.busy || !doc || !d.last) return;
  d.busy = true;
  const asked = d.last;
  try {
    const s = await invoke<Selection>("select_text", { doc: doc.id, page: d.page, a: d.start, b: asked });
    if (drag === d) {
      sel = s.rects.length ? { page: d.page, rects: s.rects, text: s.text } : null;
      if (sel) drawSelection();
      else pages[d.page].layer.querySelectorAll(".sel").forEach((e) => e.remove());
    }
  } catch { /* stale document etc. */ }
  d.busy = false;
  if (drag === d && d.last !== asked) void updateSelection();
}

pagesEl.addEventListener("mousedown", (e) => {
  if (e.button !== 0 || !doc) return;
  const pageEl = (e.target as HTMLElement).closest<HTMLElement>(".page");
  if (!pageEl) return;
  e.preventDefault(); // no native image dragging / selection
  scroller.focus({ preventScroll: true });
  closeNoteEditor();
  const i = +pageEl.dataset.i!;
  if (noteTool) {
    const [x, y] = toPdf(i, e);
    openNoteEditor({ mode: "sticky", page: i, x, y }, e.clientX, e.clientY);
    return;
  }
  clearSelection();
  drag = { page: i, start: toPdf(i, e), last: null, busy: false, moved: false };
});

window.addEventListener("mousemove", (e) => {
  if (!drag || !doc) return;
  const pt = toPdf(drag.page, e);
  if (!drag.moved && Math.hypot(pt[0] - drag.start[0], pt[1] - drag.start[1]) < 3) return;
  drag.moved = true;
  drag.last = pt;
  void updateSelection();
});

window.addEventListener("mouseup", async (e) => {
  const d = drag;
  if (!d) return;
  if (d.moved && doc) {
    d.last = toPdf(d.page, e);
    // one final, definitive selection for where the mouse was released
    while (d.busy) await new Promise((r) => setTimeout(r, 10));
    drag = null;
    try {
      const s = await invoke<Selection>("select_text", { doc: doc.id, page: d.page, a: d.start, b: d.last });
      sel = s.rects.length ? { page: d.page, rects: s.rects, text: s.text } : null;
    } catch { sel = null; }
    drawSelection();
    if (sel) showSelTool(e.clientX, e.clientY);
  } else {
    drag = null;
  }
});

function place(el: HTMLElement, x: number, y: number) {
  el.style.left = `${Math.max(8, Math.min(x, window.innerWidth - el.offsetWidth - 8))}px`;
  el.style.top = `${Math.max(8, Math.min(y, window.innerHeight - el.offsetHeight - 8))}px`;
}
function showSelTool(x: number, y: number) {
  seltool.hidden = false;
  place(seltool, x - seltool.offsetWidth / 2, y + 14);
}

async function applyMarkup(kind: "highlight" | "underline" | "strikeout", rgb: [number, number, number], note?: string, from?: { page: number; rects: Rect[] }) {
  const target = from ?? sel;
  if (!doc || !target) return;
  const { page, rects } = target;
  try {
    await invoke("add_markup", { doc: doc.id, page, kind, rects, rgb, note: note ?? null });
  } catch (e) {
    toast(errText(e));
    return;
  }
  clearSelection();
  await afterEdit(page);
}

seltool.addEventListener("mousedown", (e) => e.preventDefault()); // keep the selection alive
seltool.addEventListener("click", (e) => {
  const b = (e.target as HTMLElement).closest<HTMLElement>("button");
  if (!b || !sel) return;
  if (b.dataset.color !== undefined) void applyMarkup("highlight", COLORS[+b.dataset.color]);
  else if (b.dataset.act === "underline") void applyMarkup("underline", [220, 40, 40]);
  else if (b.dataset.act === "strikeout") void applyMarkup("strikeout", [220, 40, 40]);
  else if (b.dataset.act === "copy") void copySelection();
  else if (b.dataset.act === "comment") {
    const r = seltool.getBoundingClientRect();
    openNoteEditor({ mode: "highlight", page: sel.page, rects: sel.rects }, r.left, r.bottom + 6);
  }
});

async function copySelection() {
  if (!sel) return;
  try {
    await navigator.clipboard.writeText(sel.text);
    toast("Copied");
  } catch {
    toast("Could not copy");
  }
  seltool.hidden = true;
}

function setNoteTool(on: boolean) {
  noteTool = on;
  $("tool-note").classList.toggle("active", on);
  pagesEl.classList.toggle("note-tool", on);
  if (on) clearSelection();
}

function openNoteEditor(ctx: NoteCtx, x: number, y: number) {
  noteCtx = ctx;
  noteText.value = ctx.mode === "edit" ? (ctx.annot.contents ?? "") : "";
  noteEditor.hidden = false;
  place(noteEditor, x, y);
  noteText.focus();
}
function closeNoteEditor() {
  noteEditor.hidden = true;
  noteCtx = null;
}

async function saveNoteEditor() {
  const ctx = noteCtx;
  if (!ctx || !doc) return;
  const text = noteText.value.trim();
  closeNoteEditor();
  if (ctx.mode === "sticky") {
    setNoteTool(false);
    if (!text) return;
    try {
      await invoke("add_note", { doc: doc.id, page: ctx.page, x: ctx.x, y: ctx.y, text });
    } catch (e) { return void toast(errText(e)); }
    await afterEdit(ctx.page);
  } else if (ctx.mode === "highlight") {
    await applyMarkup("highlight", COLORS[0], text || undefined, { page: ctx.page, rects: ctx.rects });
  } else {
    try {
      await invoke("set_annot_contents", { doc: doc.id, page: ctx.annot.page, annot: ctx.annot.index, text });
    } catch (e) { return void toast(errText(e)); }
    await afterEdit(ctx.annot.page);
  }
}
$("note-save").onclick = () => void saveNoteEditor();
$("note-cancel").onclick = () => {
  if (noteCtx?.mode === "sticky") setNoteTool(false);
  closeNoteEditor();
};
noteText.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
    e.preventDefault();
    void saveNoteEditor();
  }
});
$("tool-note").onclick = () => setNoteTool(!noteTool);

/** Refreshes one page after its annotations changed, and schedules a save. */
async function afterEdit(page: number) {
  if (!doc) return;
  try {
    const batch = await invoke<Annot[]>("annotations", { doc: doc.id, from: page, to: page + 1 });
    annots = annots.filter((a) => a.page !== page).concat(batch);
    annots.sort((a, b) => a.page - b.page || b.rect[3] - a.rect[3]);
    renderComments();
  } catch { /* ignore */ }
  renderVer++;
  const p = pages[page];
  if (p) {
    p.loading = null;
    p.key = null;
    pump();
  }
  dirty = true;
  setSaveState("Unsaved", false);
  clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => void saveNow(), 900);
}

function setSaveState(text: string, err: boolean) {
  saveState.textContent = text;
  saveState.classList.toggle("err", err);
}

/** Writes pending edits into the PDF (as an incremental update Adobe can read). */
function saveNow(): Promise<void> {
  clearTimeout(saveTimer);
  saveChain = saveChain.then(async () => {
    if (!dirty || !doc) return;
    const id = doc.id;
    dirty = false;
    setSaveState("Saving…", false);
    try {
      const wrote = await invoke<boolean>("save_doc", { doc: id });
      setSaveState(wrote ? "Saved" : "", false);
      if (wrote) window.setTimeout(() => !dirty && setSaveState("", false), 2500);
    } catch (e) {
      dirty = true;
      setSaveState("Not saved", true);
      toast(errText(e));
    }
  });
  return saveChain;
}

commentList.addEventListener("click", async (ev) => {
  const t = ev.target as HTMLElement;
  const btn = t.closest<HTMLElement>("button[data-act]");
  const c = t.closest<HTMLElement>(".c");
  if (!btn || !c || !doc) return;
  const a = annots[+c.dataset.i!];
  if (btn.dataset.act === "del") {
    if (!confirm("Delete this comment from the PDF?")) return;
    try {
      await invoke("delete_annot", { doc: doc.id, page: a.page, annot: a.index });
    } catch (e) { return void toast(errText(e)); }
    await afterEdit(a.page);
  } else if (btn.dataset.act === "edit") {
    const r = c.getBoundingClientRect();
    openNoteEditor({ mode: "edit", annot: a }, r.left - 316 > 0 ? r.left - 316 : r.left, r.top);
  }
});

/* ---------- toolbar / keyboard ---------- */

function updateNav() {
  const i = orderedFiles.findIndex((f) => f.path === docPath);
  ($("prev-pdf") as HTMLButtonElement).disabled = i <= 0;
  ($("next-pdf") as HTMLButtonElement).disabled = i < 0 || i >= orderedFiles.length - 1;
}

/** Opens the next/previous PDF in tree order, skipping any that cannot be opened. */
let stepping = false;
async function step(dir: -1 | 1) {
  if (stepping) return;
  stepping = true;
  try {
    let i = orderedFiles.findIndex((f) => f.path === docPath);
    if (i < 0) return;
    let skipped = 0;
    for (i += dir; i >= 0 && i < orderedFiles.length; i += dir) {
      const r = await openFile(orderedFiles[i].path, 0, [], [], true);
      if (r === "stale") return;
      if (r === "ok") {
        if (skipped) toast(`Skipped ${skipped} PDF${skipped === 1 ? "" : "s"} that could not be opened.`);
        return;
      }
      skipped++;
    }
    toast(skipped ? `No more readable PDFs (skipped ${skipped}).` : dir > 0 ? "That was the last PDF." : "That was the first PDF.");
  } finally {
    stepping = false;
  }
}

async function openDialog() {
  const picked = await open({ multiple: false, filters: [{ name: "PDF", extensions: ["pdf"] }] });
  if (typeof picked === "string") void openFile(picked, 0);
}

function toggleComments(force?: boolean) {
  const show = force ?? commentsEl.hidden;
  commentsEl.hidden = !show;
  try { localStorage.setItem("comments", show ? "1" : "0"); } catch { /* storage unavailable */ }
  if (fitWidth) window.setTimeout(() => setZoom(computeFit(), true), 0);
}

function toggleSidebar() {
  app.classList.toggle("no-side");
  if (fitWidth) window.setTimeout(() => setZoom(computeFit(), true), 0);
}

$("choose-folder").onclick = chooseFolder;
$("rescan").onclick = () => {
  lastRescan = Date.now();
  indexing = true;
  void invoke("rescan", { retryErrors: true });
};
$("open-file").onclick = openDialog;
$("toggle-side").onclick = toggleSidebar;
$("toggle-comments").onclick = () => toggleComments();
$("prev-pdf").onclick = () => step(-1);
$("next-pdf").onclick = () => step(1);
$("zoom-in").onclick = () => setZoom(zoom * 1.15);
$("zoom-out").onclick = () => setZoom(zoom / 1.15);
$("zoom-fit").onclick = () => setZoom(computeFit(), true);
q.addEventListener("input", runSearchSoon);
pageInput.addEventListener("keydown", (e) => {
  if (e.key !== "Enter") return;
  const n = parseInt(pageInput.value, 10);
  if (doc && n >= 1 && n <= doc.page_count) goToPage(n - 1);
  scroller.focus();
});
pageInput.addEventListener("focus", () => pageInput.select());

scroller.addEventListener(
  "wheel",
  (e) => {
    if (!e.ctrlKey) return;
    e.preventDefault();
    setZoom(zoom * (e.deltaY < 0 ? 1.1 : 1 / 1.1));
  },
  { passive: false },
);

window.addEventListener("keydown", (e) => {
  const key = e.key.toLowerCase();
  if (e.ctrlKey && (key === "k" || (e.shiftKey && key === "f"))) {
    e.preventDefault();
    if (app.classList.contains("no-side")) toggleSidebar();
    q.focus();
    q.select();
  } else if (e.ctrlKey && key === "o") {
    e.preventDefault();
    void openDialog();
  } else if (e.ctrlKey && key === "b") {
    e.preventDefault();
    toggleSidebar();
  } else if (e.ctrlKey && (key === "=" || key === "+")) {
    e.preventDefault();
    setZoom(zoom * 1.15);
  } else if (e.ctrlKey && key === "-") {
    e.preventDefault();
    setZoom(zoom / 1.15);
  } else if (e.ctrlKey && key === "0") {
    e.preventDefault();
    setZoom(computeFit(), true);
  } else if (e.ctrlKey && key === "1") {
    e.preventDefault();
    setZoom(1);
  } else if (e.altKey && (key === "arrowright" || key === "arrowleft")) {
    e.preventDefault();
    step(key === "arrowright" ? 1 : -1);
  } else if (e.ctrlKey && key === "s") {
    e.preventDefault();
    void saveNow();
  } else if (e.ctrlKey && key === "c" && sel && !(e.target instanceof HTMLInputElement) && !(e.target instanceof HTMLTextAreaElement)) {
    e.preventDefault();
    void copySelection();
  } else if (key === "escape" && !noteEditor.hidden) {
    $("note-cancel").click();
  } else if (key === "escape" && (sel || noteTool)) {
    clearSelection();
    setNoteTool(false);
  } else if (key === "n" && !e.ctrlKey && !e.altKey && !(e.target instanceof HTMLInputElement) && !(e.target instanceof HTMLTextAreaElement)) {
    setNoteTool(!noteTool);
  } else if (key === "escape" && document.activeElement === q) {
    if (q.value) { q.value = ""; void runSearch(); } else scroller.focus();
  } else if (key === "enter" && document.activeElement === q) {
    const first = lastResults[0];
    if (first && q.value.trim()) void openFile(first.path, (first.hits[0]?.page ?? 1) - 1, termsFromQuery(q.value), first.hits.map((h) => h.page - 1));
  }
});

window.addEventListener(
  "resize",
  debounce(() => {
    if (fitWidth && doc) setZoom(computeFit(), true);
  }, 150),
);
window.addEventListener("focus", () => {
  if (settings.folder && Date.now() - lastRescan > 20_000) {
    lastRescan = Date.now();
    void invoke("rescan", { retryErrors: false });
  }
});

/* ---------- startup ---------- */

async function start() {
  settings = await invoke<Settings>("get_settings");
  showFolder();
  loadTreeState();
  try {
    if (localStorage.getItem("comments") === "1") commentsEl.hidden = false;
  } catch { /* storage unavailable */ }
  renderComments();
  showMessage("Choose a PDF from the sidebar, or press Ctrl+O.");
  if (settings.folder) {
    await refreshFiles();
    await showSummary();
  } else {
    renderEmptyFolder();
  }
  const launch = await invoke<string | null>("launch_file");
  if (launch) await openFile(launch, 0);
  else if (settings.last_file) await openFile(settings.last_file, settings.last_page ?? 0);
}

// The window starts hidden; reveal it only once the interface has real content.
// (The Rust side also shows it after a few seconds in case this never runs.)
void start()
  .catch((e) => toast(errText(e)))
  .finally(() => requestAnimationFrame(() => void getCurrentWindow().show().catch(() => {})));
