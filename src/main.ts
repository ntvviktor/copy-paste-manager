import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ClipEntry, Filter, PermissionStatus } from "./types";

/**
 * All keyboard input is handled by this one global listener, including search
 * typing — the search box is readonly and purely a display element.
 *
 * That is deliberate. The popup is a non-activating NSPanel, so ClipVim is never
 * the active application; relying on a natively focused <input> to collect
 * keystrokes in that state is fragile. Owning the keymap ourselves also means
 * `j`/`k` and `/` behave identically no matter what the DOM thinks is focused.
 */

const FILTERS: Filter[] = ["all", "text", "image"];
const HALF_PAGE = 5;

let entries: ClipEntry[] = [];
let visible: ClipEntry[] = [];
let selected = 0;
let query = "";
let filter: Filter = "all";
let mode: "list" | "search" = "list";
let pendingG = false;
let helpOpen = false;

const el = {
  list: must<HTMLUListElement>("list"),
  preview: must<HTMLElement>("preview"),
  searchInput: must<HTMLInputElement>("search-input"),
  searchIcon: must<HTMLElement>("search-icon"),
  filterChip: must<HTMLElement>("filter-chip"),
  count: must<HTMLElement>("count"),
  help: must<HTMLElement>("help"),
  banner: must<HTMLElement>("permission-banner"),
  grant: must<HTMLButtonElement>("grant-button"),
};

function must<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing element #${id}`);
  return node as T;
}

const toast = document.createElement("div");
toast.className = "toast";
toast.hidden = true;
document.body.appendChild(toast);

let toastTimer: number | undefined;
function showError(message: string) {
  toast.textContent = message;
  toast.hidden = false;
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => {
    toast.hidden = true;
  }, 3200);
}

// ---------------------------------------------------------------- data

async function refresh() {
  try {
    entries = await invoke<ClipEntry[]>("list_history");
  } catch (err) {
    showError(String(err));
    entries = [];
  }
  applyFilter({ keepSelection: true });
}

function applyFilter({ keepSelection }: { keepSelection: boolean }) {
  const previousId = keepSelection ? visible[selected]?.id : undefined;
  const needle = query.trim().toLowerCase();

  visible = entries.filter((entry) => {
    if (filter !== "all" && entry.kind !== filter) return false;
    if (!needle) return true;
    if (entry.kind === "image") return "image".includes(needle);
    return entry.preview.toLowerCase().includes(needle);
  });

  const restored = previousId
    ? visible.findIndex((entry) => entry.id === previousId)
    : -1;
  selected = restored >= 0 ? restored : 0;
  render();
}

// ---------------------------------------------------------------- rendering

function render() {
  el.searchInput.value = query;
  el.searchIcon.textContent = mode === "search" ? "/" : "⌘";
  el.searchInput.placeholder =
    mode === "search" ? "Type to filter…" : "Press / to search";
  el.filterChip.textContent = filter;
  el.count.textContent = visible.length
    ? `${selected + 1} / ${visible.length}`
    : "0";
  document.body.classList.toggle("searching", mode === "search");

  el.list.replaceChildren(
    ...visible.map((entry, index) => renderRow(entry, index)),
  );

  if (!visible.length) {
    const empty = document.createElement("li");
    empty.className = "empty";
    empty.textContent = entries.length
      ? "Nothing matches that filter."
      : "Clipboard history is empty. Copy something.";
    el.list.appendChild(empty);
  }

  renderPreview(visible[selected]);

  const active = el.list.children[selected] as HTMLElement | undefined;
  active?.scrollIntoView({ block: "nearest" });
}

function renderRow(entry: ClipEntry, index: number): HTMLLIElement {
  const row = document.createElement("li");
  row.className = "row";
  row.setAttribute("role", "option");
  row.setAttribute("aria-selected", String(index === selected));
  if (index === selected) row.classList.add("selected");

  const ordinal = document.createElement("span");
  ordinal.className = "ordinal";
  ordinal.textContent = index < 9 ? String(index + 1) : "";
  row.appendChild(ordinal);

  if (entry.kind === "image" && entry.thumb) {
    const img = document.createElement("img");
    img.className = "thumb";
    img.src = entry.thumb;
    img.alt = entry.preview;
    row.appendChild(img);
  } else {
    const glyph = document.createElement("span");
    glyph.className = "glyph";
    glyph.textContent = entry.kind === "image" ? "▣" : "¶";
    row.appendChild(glyph);
  }

  const label = document.createElement("span");
  label.className = "label";
  label.textContent = oneLine(entry.preview);
  row.appendChild(label);

  const meta = document.createElement("span");
  meta.className = "meta";
  meta.textContent = relativeTime(entry.createdAt);
  row.appendChild(meta);

  row.addEventListener("click", () => {
    selected = index;
    void pasteSelected();
  });
  row.addEventListener("mouseenter", () => {
    if (selected !== index) {
      selected = index;
      render();
    }
  });

  return row;
}

function renderPreview(entry: ClipEntry | undefined) {
  el.preview.replaceChildren();
  if (!entry) return;

  if (entry.kind === "image" && entry.thumb) {
    const img = document.createElement("img");
    img.className = "preview-image";
    img.src = entry.thumb;
    img.alt = entry.preview;
    el.preview.appendChild(img);
  } else {
    const body = document.createElement("pre");
    body.className = "preview-text";
    body.textContent = entry.preview + (entry.truncated ? "\n…" : "");
    el.preview.appendChild(body);
  }

  const meta = document.createElement("div");
  meta.className = "preview-meta";
  meta.textContent =
    entry.kind === "image"
      ? `${entry.preview} · ${formatBytes(entry.byteLen)} PNG`
      : `${entry.charCount.toLocaleString()} chars · ${entry.lineCount} line${
          entry.lineCount === 1 ? "" : "s"
        }`;
  el.preview.appendChild(meta);
}

const HELP_ROWS: [string, string][] = [
  ["j / ↓ / ⌃n", "next entry"],
  ["k / ↑ / ⌃p", "previous entry"],
  ["gg / G", "first / last entry"],
  ["⌃d / ⌃u", "half page down / up"],
  ["1 – 9", "jump to entry"],
  ["⏎", "paste into the app underneath"],
  ["y", "copy without pasting"],
  ["d / x", "delete entry"],
  ["/", "search"],
  ["⇥ / ⇧⇥", "cycle filter"],
  ["?", "toggle this help"],
  ["esc", "close"],
];

function renderHelp() {
  el.help.hidden = !helpOpen;
  if (!helpOpen) return;
  el.help.replaceChildren(
    ...HELP_ROWS.map(([keys, description]) => {
      const row = document.createElement("div");
      row.className = "help-row";
      const k = document.createElement("kbd");
      k.textContent = keys;
      const d = document.createElement("span");
      d.textContent = description;
      row.append(k, d);
      return row;
    }),
  );
}

// ---------------------------------------------------------------- actions

function move(delta: number) {
  if (!visible.length) return;
  selected = Math.min(Math.max(selected + delta, 0), visible.length - 1);
  render();
}

function jumpTo(index: number) {
  if (!visible.length) return;
  selected = Math.min(Math.max(index, 0), visible.length - 1);
  render();
}

async function pasteSelected() {
  const entry = visible[selected];
  if (!entry) return;
  try {
    await invoke("paste_entry", { id: entry.id });
  } catch (err) {
    showError(String(err));
  }
}

async function copySelected() {
  const entry = visible[selected];
  if (!entry) return;
  try {
    await invoke("copy_entry", { id: entry.id });
  } catch (err) {
    showError(String(err));
  }
}

async function deleteSelected() {
  const entry = visible[selected];
  if (!entry) return;
  try {
    await invoke("delete_entry", { id: entry.id });
    entries = entries.filter((item) => item.id !== entry.id);
    // Keep the cursor where it was rather than snapping back to the top.
    const previous = selected;
    applyFilter({ keepSelection: false });
    jumpTo(Math.min(previous, visible.length - 1));
  } catch (err) {
    showError(String(err));
  }
}

function cycleFilter(step: number) {
  const next = (FILTERS.indexOf(filter) + step + FILTERS.length) % FILTERS.length;
  filter = FILTERS[next];
  applyFilter({ keepSelection: true });
}

function enterSearch() {
  mode = "search";
  render();
}

function leaveSearch({ clear }: { clear: boolean }) {
  mode = "list";
  if (clear && query) {
    query = "";
    applyFilter({ keepSelection: true });
  } else {
    render();
  }
}

async function close() {
  try {
    await invoke("hide_popup");
  } catch (err) {
    showError(String(err));
  }
}

// ---------------------------------------------------------------- keymap

window.addEventListener("keydown", (event) => {
  // Never let the browser's own shortcuts or caret handling interfere.
  const key = event.key;
  const ctrl = event.ctrlKey;

  if (key === "Escape") {
    event.preventDefault();
    if (helpOpen) {
      helpOpen = false;
      renderHelp();
    } else if (mode === "search") {
      leaveSearch({ clear: true });
    } else {
      void close();
    }
    return;
  }

  if (key === "Enter") {
    event.preventDefault();
    void pasteSelected();
    return;
  }

  // Navigation that works in both modes.
  if (key === "ArrowDown" || (ctrl && key.toLowerCase() === "n")) {
    event.preventDefault();
    move(1);
    return;
  }
  if (key === "ArrowUp" || (ctrl && key.toLowerCase() === "p")) {
    event.preventDefault();
    move(-1);
    return;
  }
  if (ctrl && key.toLowerCase() === "d") {
    event.preventDefault();
    move(HALF_PAGE);
    return;
  }
  if (ctrl && key.toLowerCase() === "u") {
    event.preventDefault();
    move(-HALF_PAGE);
    return;
  }

  if (mode === "search") {
    if (key === "Backspace") {
      event.preventDefault();
      query = query.slice(0, -1);
      applyFilter({ keepSelection: true });
      return;
    }
    if (key === "Tab") {
      event.preventDefault();
      cycleFilter(event.shiftKey ? -1 : 1);
      return;
    }
    // Collect printable characters ourselves.
    if (key.length === 1 && !ctrl && !event.metaKey && !event.altKey) {
      event.preventDefault();
      query += key;
      applyFilter({ keepSelection: true });
    }
    return;
  }

  // ---- list mode ----
  if (key === "Tab") {
    event.preventDefault();
    cycleFilter(event.shiftKey ? -1 : 1);
    return;
  }

  // `gg` — only a bare `g` arms it, and anything else disarms it.
  if (key === "g" && !ctrl) {
    event.preventDefault();
    if (pendingG) {
      pendingG = false;
      jumpTo(0);
    } else {
      pendingG = true;
    }
    return;
  }
  const wasPendingG = pendingG;
  pendingG = false;

  switch (key) {
    case "j":
      event.preventDefault();
      move(1);
      return;
    case "k":
      event.preventDefault();
      move(-1);
      return;
    case "G":
      event.preventDefault();
      jumpTo(visible.length - 1);
      return;
    case "y":
      event.preventDefault();
      void copySelected();
      return;
    case "d":
    case "x":
      event.preventDefault();
      void deleteSelected();
      return;
    case "/":
      event.preventDefault();
      enterSearch();
      return;
    case "?":
      event.preventDefault();
      helpOpen = !helpOpen;
      renderHelp();
      return;
    default:
      break;
  }

  if (/^[1-9]$/.test(key) && !ctrl && !wasPendingG) {
    event.preventDefault();
    jumpTo(Number(key) - 1);
  }
});

// The panel is chrome-free; suppress the context menu and text selection drag.
window.addEventListener("contextmenu", (event) => event.preventDefault());

el.grant.addEventListener("click", () => {
  void invoke("open_accessibility_settings").catch((err) =>
    showError(String(err)),
  );
});

// ---------------------------------------------------------------- helpers

function oneLine(text: string): string {
  return text.replace(/\s+/g, " ").trim();
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function relativeTime(epochMs: number): string {
  const seconds = Math.max(0, Math.round((Date.now() - epochMs) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.round(hours / 24)}d`;
}

async function checkPermissions() {
  try {
    const status = await invoke<PermissionStatus>("permission_status");
    el.banner.hidden = !status.required || status.accessibility;
  } catch {
    el.banner.hidden = true;
  }
}

// ---------------------------------------------------------------- startup

void listen("history-changed", () => void refresh());

void listen("popup-shown", () => {
  // Reappear in a known state rather than wherever the user left off.
  query = "";
  mode = "list";
  filter = "all";
  pendingG = false;
  helpOpen = false;
  renderHelp();
  void refresh();
  void checkPermissions();
});

void refresh();
void checkPermissions();
