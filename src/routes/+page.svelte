<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { convertFileSrc } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { listen } from "@tauri-apps/api/event";
  import { open } from "@tauri-apps/plugin-dialog";
  import { onMount, tick } from "svelte";
  import { fade, scale } from "svelte/transition";
  import { flip } from "svelte/animate";
  import { quintOut } from "svelte/easing";

  type App = {
    id: number;
    title: string;
    path: string;
    icon: string | null;
    group: string | null;
    type: string | null;
  };

  type Section = { name: string; apps: App[] };

  type BgMode = "mica";

  type Settings = {
    iconSize: number;
    labelSize: number;
    tileMin: number;
    bgMode: BgMode;
    fullscreen: boolean;
    groupOrder: string[];
    hotkey: string;
    accent: string;
  };

  const DEFAULT_SETTINGS: Settings = {
    iconSize: 44,
    labelSize: 10.5,
    tileMin: 72,
    bgMode: "mica",
    fullscreen: false,
    groupOrder: [],
    hotkey: "Alt+Space",
    accent: "#8ab4ff",
  };

  // Parse #rrggbb → "r, g, b" for rgba(var(--accent-rgb), …) usage in CSS.
  // Invalid input falls back to the default accent so a corrupted setting
  // never produces an unrenderable var.
  function hexToRgb(hex: string): string {
    const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
    if (!m) return "138, 180, 255";
    const n = parseInt(m[1], 16);
    return `${(n >> 16) & 0xff}, ${(n >> 8) & 0xff}, ${n & 0xff}`;
  }

  function isValidHex(hex: string): boolean {
    return /^#[0-9a-f]{6}$/i.test(hex.trim());
  }

  function loadSettings(): Settings {
    try {
      const raw = localStorage.getItem("wisplet.settings");
      if (!raw) return { ...DEFAULT_SETTINGS };
      const parsed = JSON.parse(raw);
      // Migrate legacy bgMode values (acrylic/overlay → mica)
      if (parsed.bgMode !== "mica") parsed.bgMode = "mica";
      if (!Array.isArray(parsed.groupOrder)) parsed.groupOrder = [];
      if (typeof parsed.hotkey !== "string" || !parsed.hotkey.trim()) parsed.hotkey = DEFAULT_SETTINGS.hotkey;
      if (typeof parsed.accent !== "string" || !isValidHex(parsed.accent)) parsed.accent = DEFAULT_SETTINGS.accent;
      return { ...DEFAULT_SETTINGS, ...parsed };
    } catch {
      return { ...DEFAULT_SETTINGS };
    }
  }

  let apps = $state<App[]>([]);
  let query = $state("");
  let mounted = $state(false);
  let visible = $state(false);
  let columnCount = $state(4);
  let loadError = $state<string | null>(null);
  let settings = $state<Settings>(loadSettings());
  let settingsOpen = $state(false);

  // Hotkey state — `activeHotkey` mirrors what Rust actually registered (cascade
  // pick or restored user choice). `settings.hotkey` is the user's saved
  // preference and only changes when the user explicitly applies one via the UI.
  // If the saved choice was unavailable at boot, `hotkeyFallbackNotice` carries
  // the (saved, active) pair so the launcher banner can tell the user.
  let activeHotkey = $state<string | null>(null);
  let hotkeyError = $state<string | null>(null);
  let hotkeyConflict = $state(false);
  let hotkeyFallbackNotice = $state<{ saved: string; active: string } | null>(null);
  let capturingHotkey = $state(false);
  let captureBuffer = $state("");
  let manualCombo = $state("");

  let searchInput = $state<HTMLInputElement | null>(null);

  // ===== App editor (add / edit / delete) =====
  // `editorDraft` is a working copy — mutated freely, only merged into `apps`
  // on save. Context menu and delete confirmation are transient overlays.
  let editorOpen = $state(false);
  let editorMode = $state<"add" | "edit">("add");
  let editorDraft = $state<App | null>(null);
  let editorBusy = $state(false);
  let editorError = $state<string | null>(null);
  // `app` set → menu on a tile (Modifier/Supprimer). `group` carries the
  // pre-fill for "Ajouter une app" — the card's group, or null in empty space.
  let contextMenu = $state<{ x: number; y: number; app: App | null; group: string | null } | null>(null);
  let pendingDelete = $state<App | null>(null);

  // Persist settings
  $effect(() => {
    if (mounted) {
      localStorage.setItem("wisplet.settings", JSON.stringify(settings));
    }
  });

  function resetSettings() {
    settings = { ...DEFAULT_SETTINGS };
  }

  async function applyBg(_mode: BgMode) {
    const win = getCurrentWindow();
    try {
      await win.setEffects({ effects: ["mica"], state: "active" });
    } catch (e) {
      console.warn("setEffects failed:", e);
    }
  }

  $effect(() => {
    if (mounted) applyBg(settings.bgMode);
  });

  // Sync "cover screen" pref with the Rust backend. Backend resizes the window to
  // the current monitor (or back to windowed default) on every show — and immediately
  // if the window is currently visible.
  $effect(() => {
    invoke("set_fullsize_pref", { value: settings.fullscreen }).catch((e) =>
      console.warn("set_fullsize_pref failed:", e),
    );
  });


  function groupApps(list: App[]): Section[] {
    const map = new Map<string, App[]>();
    for (const a of list) {
      const g = a.group?.trim() || "Autres";
      if (!map.has(g)) map.set(g, []);
      map.get(g)!.push(a);
    }
    const sections = [...map.entries()].map(([name, apps]) => ({
      name,
      apps: apps.slice().sort((a, b) => a.title.localeCompare(b.title)),
    }));
    const order = settings.groupOrder;
    const orderIdx = new Map(order.map((n, i) => [n, i]));
    return sections.sort((a, b) => {
      const ia = orderIdx.has(a.name) ? (orderIdx.get(a.name) as number) : Infinity;
      const ib = orderIdx.has(b.name) ? (orderIdx.get(b.name) as number) : Infinity;
      if (ia !== ib) return ia - ib;
      return a.name.localeCompare(b.name);
    });
  }

  // ===== Drag & drop reordering of group sections =====
  let draggedGroup = $state<string | null>(null);
  let dropTarget = $state<{ name: string; pos: "before" | "after" } | null>(null);

  function onGroupDragStart(e: DragEvent, name: string) {
    if (query.trim()) {
      e.preventDefault();
      return;
    }
    draggedGroup = name;
    if (e.dataTransfer) {
      e.dataTransfer.effectAllowed = "move";
      e.dataTransfer.setData("text/plain", name);
    }
  }

  function onGroupDragOver(e: DragEvent, name: string) {
    if (!draggedGroup || draggedGroup === name) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
    const target = e.currentTarget as HTMLElement;
    const rect = target.getBoundingClientRect();
    const pos: "before" | "after" = e.clientY < rect.top + rect.height / 2 ? "before" : "after";
    if (!dropTarget || dropTarget.name !== name || dropTarget.pos !== pos) {
      dropTarget = { name, pos };
    }
  }

  function onGroupDragLeave(e: DragEvent, name: string) {
    // Only clear if we're actually leaving (not entering a child)
    const related = e.relatedTarget as Node | null;
    if (related && (e.currentTarget as Node).contains(related)) return;
    if (dropTarget?.name === name) dropTarget = null;
  }

  function onGroupDrop(e: DragEvent, name: string) {
    e.preventDefault();
    if (!draggedGroup || draggedGroup === name) {
      draggedGroup = null;
      dropTarget = null;
      return;
    }
    const allNames = filteredSections.map((s) => s.name);
    const current = settings.groupOrder.length
      ? [...settings.groupOrder.filter((n) => allNames.includes(n)), ...allNames.filter((n) => !settings.groupOrder.includes(n))]
      : [...allNames];
    const from = current.indexOf(draggedGroup);
    if (from === -1) return;
    current.splice(from, 1);
    let to = current.indexOf(name);
    if (to === -1) return;
    const pos = dropTarget?.pos ?? "before";
    if (pos === "after") to += 1;
    current.splice(to, 0, draggedGroup);
    settings.groupOrder = current;
    draggedGroup = null;
    dropTarget = null;
  }

  function onGroupDragEnd() {
    draggedGroup = null;
    dropTarget = null;
  }

  function resetGroupOrder() {
    settings.groupOrder = [];
  }

  // ===== Hotkey rebind =====
  // Modifier-only codes — pressing just Ctrl/Alt/Shift/Super is not a valid combo.
  const MODIFIER_CODES = new Set([
    "ControlLeft", "ControlRight",
    "AltLeft", "AltRight",
    "ShiftLeft", "ShiftRight",
    "MetaLeft", "MetaRight",
    "OSLeft", "OSRight",
  ]);

  function buildCombo(e: KeyboardEvent): string | null {
    if (MODIFIER_CODES.has(e.code)) return null;
    const parts: string[] = [];
    if (e.ctrlKey) parts.push("Ctrl");
    if (e.altKey) parts.push("Alt");
    if (e.shiftKey) parts.push("Shift");
    if (e.metaKey) parts.push("Super");
    if (parts.length === 0) return null;
    parts.push(e.code);
    return parts.join("+");
  }

  // Human-friendly label: KeyA → A, Digit1 → 1, Super → Win.
  function formatComboLabel(combo: string | null): string {
    if (!combo) return "—";
    return combo
      .replace(/\bSuper\b/g, "Win")
      .replace(/\bKey([A-Z])\b/g, "$1")
      .replace(/\bDigit(\d)\b/g, "$1");
  }

  async function syncHotkey() {
    try {
      const current = await invoke<string | null>("get_current_hotkey");
      activeHotkey = current ?? null;
      hotkeyConflict = current === null;
    } catch (e) {
      console.warn("get_current_hotkey failed:", e);
    }
  }

  // Boot-time restore: try the user's saved binding first. If it's free, Rust
  // unregisters the cascade entry it grabbed at setup and switches to it. If
  // it's taken, we leave the cascade fallback alone and surface a banner so
  // the user knows their choice didn't apply this session.
  async function restoreHotkey() {
    const saved = settings.hotkey;
    if (!saved) {
      await syncHotkey();
      return;
    }
    try {
      const result = await invoke<string>("set_hotkey", { combo: saved });
      activeHotkey = result;
      hotkeyConflict = false;
      hotkeyFallbackNotice = null;
    } catch {
      const fallback = await invoke<string | null>("get_current_hotkey");
      activeHotkey = fallback;
      hotkeyConflict = fallback === null;
      if (fallback && fallback !== saved) {
        hotkeyFallbackNotice = { saved, active: fallback };
      }
    }
  }

  async function applyHotkey(combo: string) {
    hotkeyError = null;
    try {
      const result = await invoke<string>("set_hotkey", { combo });
      activeHotkey = result;
      settings.hotkey = result;
      hotkeyConflict = false;
      hotkeyFallbackNotice = null;
      capturingHotkey = false;
      captureBuffer = "";
    } catch (e) {
      hotkeyError = String(e);
    }
  }

  function startCapture() {
    capturingHotkey = true;
    captureBuffer = "";
    hotkeyError = null;
  }

  function cancelCapture() {
    capturingHotkey = false;
    captureBuffer = "";
  }

  function onCaptureKey(e: KeyboardEvent) {
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Escape" && !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey) {
      cancelCapture();
      return;
    }
    const combo = buildCombo(e);
    if (combo) captureBuffer = combo;
  }

  function onCaptureKeyUp(e: KeyboardEvent) {
    if (!capturingHotkey) return;
    if (MODIFIER_CODES.has(e.code)) return;
    if (captureBuffer) {
      applyHotkey(captureBuffer);
    }
  }

  // ===== App editor logic =====
  // Existing group names — feeds the editor's group datalist for autocomplete.
  let groupNames = $derived.by(() => {
    const set = new Set<string>();
    for (const a of apps) {
      const g = a.group?.trim();
      if (g) set.add(g);
    }
    return [...set].sort((a, b) => a.localeCompare(b));
  });

  async function persistApps() {
    try {
      await invoke("save_apps", { apps });
    } catch (e) {
      console.error("save_apps failed", e);
    }
  }

  // Suppress auto-hide while the editor is open: the native file picker steals
  // window focus, which would otherwise trigger anim-out and hide the launcher.
  async function setHideSuppressed(value: boolean) {
    try {
      await invoke("set_hide_suppressed", { value });
    } catch (e) {
      console.warn("set_hide_suppressed failed:", e);
    }
  }

  function openAddEditor(group: string | null = null) {
    editorMode = "add";
    editorDraft = { id: Date.now(), title: "", path: "", icon: null, group, type: "app" };
    editorError = null;
    contextMenu = null;
    editorOpen = true;
    setHideSuppressed(true);
  }

  function openEditEditor(app: App) {
    editorMode = "edit";
    editorDraft = { ...app };
    editorError = null;
    contextMenu = null;
    editorOpen = true;
    setHideSuppressed(true);
  }

  function closeEditor() {
    editorOpen = false;
    editorDraft = null;
    editorError = null;
    editorBusy = false;
    setHideSuppressed(false);
  }

  // Picked a path → auto-fill icon (always) and title (only when still empty,
  // so a name the user already typed is never clobbered).
  async function inspectApp(path: string) {
    if (!editorDraft) return;
    editorBusy = true;
    try {
      const info = await invoke<{ title: string; icon: string | null }>("inspect_app", { path });
      if (!editorDraft) return;
      if (info.icon) editorDraft.icon = info.icon;
      if (info.title && !editorDraft.title.trim()) editorDraft.title = info.title;
    } catch (e) {
      console.warn("inspect_app failed:", e); // keep current icon / letter fallback
    } finally {
      editorBusy = false;
    }
  }

  async function browsePath() {
    if (!editorDraft) return;
    const selected = await open({
      multiple: false,
      filters: [{ name: "Applications", extensions: ["exe", "lnk"] }],
    });
    if (typeof selected === "string") {
      editorDraft.path = selected;
      await inspectApp(selected);
    }
  }

  async function browseIcon() {
    if (!editorDraft) return;
    const selected = await open({
      multiple: false,
      filters: [{ name: "Images", extensions: ["png", "svg", "jpg", "jpeg", "ico"] }],
    });
    if (typeof selected === "string") editorDraft.icon = selected;
  }

  function clearIcon() {
    if (editorDraft) editorDraft.icon = null;
  }

  async function saveEditor() {
    if (!editorDraft) return;
    const title = editorDraft.title.trim();
    const path = editorDraft.path.trim();
    if (!title) { editorError = "Le titre est requis."; return; }
    if (!path) { editorError = "Le chemin est requis."; return; }
    const group = editorDraft.group?.trim() || null;
    const finalApp: App = { ...editorDraft, title, path, group };
    apps = editorMode === "add"
      ? [...apps, finalApp]
      : apps.map((a) => (a.id === finalApp.id ? finalApp : a));
    await persistApps();
    closeEditor();
  }

  // Three right-click zones — each stops propagation so only the most specific
  // handler fires (tile inside card inside columns).
  function onAppContextMenu(e: MouseEvent, app: App) {
    e.preventDefault();
    e.stopPropagation();
    contextMenu = { x: e.clientX, y: e.clientY, app, group: app.group ?? null };
  }

  function onCardContextMenu(e: MouseEvent, group: string) {
    e.preventDefault();
    e.stopPropagation();
    contextMenu = { x: e.clientX, y: e.clientY, app: null, group };
  }

  function onEmptyContextMenu(e: MouseEvent) {
    e.preventDefault();
    contextMenu = { x: e.clientX, y: e.clientY, app: null, group: null };
  }

  function requestDelete(app: App) {
    contextMenu = null;
    pendingDelete = app;
  }

  async function confirmDelete() {
    if (!pendingDelete) return;
    const id = pendingDelete.id;
    apps = apps.filter((a) => a.id !== id);
    pendingDelete = null;
    await persistApps();
  }

  // Filter + group
  let filteredSections = $derived.by(() => {
    const q = query.trim().toLowerCase();
    const filtered = q
      ? apps.filter((a) => a.title.toLowerCase().includes(q))
      : apps;
    return groupApps(filtered);
  });

  // Masonry: distribute sections to N columns by lowest current height
  let columns = $derived.by(() => {
    const cols: Section[][] = Array.from({ length: columnCount }, () => []);
    const heights = new Array(columnCount).fill(0);
    for (const s of filteredSections) {
      const rows = Math.ceil(s.apps.length / 4);
      const h = 56 + rows * 96;
      let min = 0;
      for (let i = 1; i < columnCount; i++) {
        if (heights[i] < heights[min]) min = i;
      }
      cols[min].push(s);
      heights[min] += h + 16;
    }
    return cols;
  });

  function updateColumnCount() {
    const w = window.innerWidth;
    if (w < 800) columnCount = 2;
    else if (w < 1200) columnCount = 3;
    else if (w < 1700) columnCount = 4;
    else columnCount = 5;
  }

  async function launch(app: App) {
    try {
      await invoke("launch_app", { path: app.path });
      await invoke("hide_window");
    } catch (e) {
      console.error("Failed to launch", app.path, e);
    }
  }

  function onKey(e: KeyboardEvent) {
    if (capturingHotkey) {
      onCaptureKey(e);
      return;
    }
    if (e.key === "Escape") {
      if (contextMenu) contextMenu = null;
      else if (pendingDelete) pendingDelete = null;
      else if (editorOpen) closeEditor();
      else if (settingsOpen) settingsOpen = false;
      else visible = false; // CSS transition out → ontransitionend → hide_window
    } else if (e.altKey && e.key === " ") {
      // Local fallback for Alt+Space when the window already has focus —
      // the global shortcut is a no-op while we own the keyboard.
      e.preventDefault();
      visible = false;
    }
  }

  function iconSrc(app: App): string {
    if (!app.icon) return "";
    return convertFileSrc(app.icon);
  }

  // Kill WebView2's default right-click menu and page-level shortcuts
  // (reload / print / back) — they break or interrupt the launcher. Reload in
  // particular tears down the Svelte app and leaves a blank shell.
  function blockContextMenu(e: MouseEvent) {
    e.preventDefault();
  }
  function blockBrowserKeys(e: KeyboardEvent) {
    const k = e.key.toLowerCase();
    const reload = e.key === "F5" || (e.ctrlKey && k === "r");
    const pageCmd = e.ctrlKey && (k === "p" || k === "s");
    const navBack = e.altKey && (e.key === "ArrowLeft" || e.key === "ArrowRight");
    if (reload || pageCmd || navBack) e.preventDefault();
  }

  onMount(async () => {
    updateColumnCount();
    window.addEventListener("resize", updateColumnCount);
    window.addEventListener("contextmenu", blockContextMenu);
    window.addEventListener("keydown", blockBrowserKeys, { capture: true });

    await applyBg(settings.bgMode);

    try {
      apps = await invoke<App[]>("load_apps");
      console.log("Loaded apps:", apps.length);
    } catch (e) {
      console.error("load_apps failed", e);
      loadError = String(e);
    }
    await tick();
    mounted = true;
    searchInput?.focus();

    const win = getCurrentWindow();

    // Tauri anim events fired by Rust on toggle / focus loss
    const unlistenIn = await win.listen<unknown>("anim-in", () => {
      query = "";
      visible = true;
      requestAnimationFrame(() => searchInput?.focus());
    });
    const unlistenOut = await win.listen<unknown>("anim-out", () => {
      visible = false;
    });

    // Re-focus search input + clear query when window regains focus
    const unlistenFocus = await win.onFocusChanged(({ payload: focused }) => {
      if (focused) {
        query = "";
        requestAnimationFrame(() => searchInput?.focus());
      }
    });

    // Backend emits this when none of the fallback hotkeys could be registered.
    const unlistenConflict = await listen("hotkey-conflict", () => {
      hotkeyConflict = true;
      settingsOpen = true;
    });

    await restoreHotkey();

    return () => {
      unlistenIn();
      unlistenOut();
      unlistenFocus();
      unlistenConflict();
      window.removeEventListener("resize", updateColumnCount);
      window.removeEventListener("contextmenu", blockContextMenu);
      window.removeEventListener("keydown", blockBrowserKeys, { capture: true });
    };
  });

  function onAnimOutEnd() {
    invoke("hide_window");
  }
</script>

<svelte:window onkeydown={onKey} onkeyup={onCaptureKeyUp} />

{#if mounted}
  <main
    class="root {settings.bgMode}"
    class:visible
    style="--icon-size: {settings.iconSize}px; --label-size: {settings.labelSize}px; --tile-min: {settings.tileMin}px; --accent: {settings.accent}; --accent-rgb: {hexToRgb(settings.accent)};"
    ontransitionend={(e) => { if (e.target === e.currentTarget && (e as TransitionEvent).propertyName === 'opacity' && !visible) onAnimOutEnd(); }}
  >
    <div class="top-bar">
      <div class="search-wrap">
        <svg class="search-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <circle cx="11" cy="11" r="7" />
          <line x1="21" y1="21" x2="16.65" y2="16.65" />
        </svg>
        <input
          bind:this={searchInput}
          bind:value={query}
          type="text"
          placeholder="Rechercher une app…"
          spellcheck="false"
          autocomplete="off"
        />
        {#if query}
          <button class="clear" onclick={() => (query = "")} aria-label="Clear">✕</button>
        {/if}
      </div>

      <button class="settings-btn" onclick={() => (settingsOpen = !settingsOpen)} aria-label="Settings" title="Paramètres">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
          <circle cx="12" cy="12" r="3" />
          <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09a1.65 1.65 0 0 0-1-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09a1.65 1.65 0 0 0 1.51-1 1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33h0a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51h0a1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82v0a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
        </svg>
      </button>
    </div>

    {#if hotkeyFallbackNotice}
      <div class="hotkey-fallback-banner" role="status">
        <span>
          Raccourci <kbd>{formatComboLabel(hotkeyFallbackNotice.saved)}</kbd>
          indisponible — Wisplet utilise <kbd>{formatComboLabel(hotkeyFallbackNotice.active)}</kbd>
          pour cette session.
        </span>
        <button
          class="banner-dismiss"
          onclick={() => (hotkeyFallbackNotice = null)}
          aria-label="Fermer"
        >✕</button>
      </div>
    {/if}

    {#if loadError}
      <div class="error-banner">
        <strong>Erreur load_apps:</strong> {loadError}
      </div>
    {:else if apps.length === 0}
      <div class="error-banner empty">Aucune app trouvée (apps.length === 0)</div>
    {/if}

    <div class="columns" oncontextmenu={onEmptyContextMenu} role="presentation">
      {#each columns as col, ci (ci)}
        <div class="column">
          {#each col as section (section.name)}
            <section
              class="card"
              class:dragging={draggedGroup === section.name}
              class:drop-before={dropTarget?.name === section.name && dropTarget.pos === "before"}
              class:drop-after={dropTarget?.name === section.name && dropTarget.pos === "after"}
              aria-label={section.name}
              in:fade={{ duration: 220, delay: 40 * ci }}
              animate:flip={{ duration: 300, easing: quintOut }}
              ondragover={(e) => onGroupDragOver(e, section.name)}
              ondragleave={(e) => onGroupDragLeave(e, section.name)}
              ondrop={(e) => onGroupDrop(e, section.name)}
              oncontextmenu={(e) => onCardContextMenu(e, section.name)}
            >
              <h3
                class="section-title"
                class:draggable={!query.trim()}
                draggable={!query.trim()}
                ondragstart={(e) => onGroupDragStart(e, section.name)}
                ondragend={onGroupDragEnd}
              >
                {section.name}
                <span class="count">{section.apps.length}</span>
              </h3>
              <div class="grid">
                {#each section.apps as app (app.id)}
                  <button
                    class="app"
                    onclick={() => launch(app)}
                    oncontextmenu={(e) => onAppContextMenu(e, app)}
                    title={app.title}
                    animate:flip={{ duration: 300, easing: quintOut }}
                  >
                    <div class="icon-wrap">
                      {#if app.icon}
                        <img src={iconSrc(app)} alt={app.title} loading="lazy" />
                      {:else}
                        <div class="icon-fallback">{app.title[0]}</div>
                      {/if}
                    </div>
                    <span class="label">{app.title}</span>
                  </button>
                {/each}
              </div>
            </section>
          {/each}
        </div>
      {/each}
    </div>

    {#if settingsOpen}
      <div
        class="drawer-backdrop"
        onclick={() => (settingsOpen = false)}
        transition:fade={{ duration: 120 }}
        role="presentation"
      ></div>
      <aside
        class="drawer"
        transition:fade={{ duration: 160 }}
      >
        <div class="drawer-header">
          <h2>Paramètres</h2>
          <button class="drawer-close" onclick={() => (settingsOpen = false)} aria-label="Close">✕</button>
        </div>

        <div class="drawer-body">
          <section class="setting-group">
            <h3>Apparence</h3>

            <label class="setting-row">
              <span class="setting-label">Taille des icônes</span>
              <span class="setting-value">{settings.iconSize}px</span>
              <input
                type="range"
                min="28"
                max="96"
                step="2"
                bind:value={settings.iconSize}
              />
            </label>

            <label class="setting-row">
              <span class="setting-label">Taille du label</span>
              <span class="setting-value">{settings.labelSize}px</span>
              <input
                type="range"
                min="9"
                max="16"
                step="0.5"
                bind:value={settings.labelSize}
              />
            </label>

            <label class="setting-row">
              <span class="setting-label">Largeur min. tuile</span>
              <span class="setting-value">{settings.tileMin}px</span>
              <input
                type="range"
                min="60"
                max="140"
                step="4"
                bind:value={settings.tileMin}
              />
            </label>

            <label class="setting-row toggle-row">
              <span class="setting-label">Plein écran</span>
              <span class="switch" class:on={settings.fullscreen}>
                <input
                  type="checkbox"
                  bind:checked={settings.fullscreen}
                />
                <span class="switch-track"></span>
                <span class="switch-thumb"></span>
              </span>
            </label>

            <label class="setting-row toggle-row">
              <span class="setting-label">Couleur d'accent</span>
              <span class="accent-picker">
                <span class="accent-hex">{settings.accent.toUpperCase()}</span>
                <input
                  type="color"
                  bind:value={settings.accent}
                  aria-label="Couleur d'accent"
                />
                <button
                  class="accent-reset"
                  onclick={(e) => { e.preventDefault(); settings.accent = DEFAULT_SETTINGS.accent; }}
                  disabled={settings.accent.toLowerCase() === DEFAULT_SETTINGS.accent.toLowerCase()}
                  aria-label="Réinitialiser la couleur d'accent"
                  title="Réinitialiser"
                >↺</button>
              </span>
            </label>
          </section>

          <section class="setting-group">
            <h3>Apps</h3>
            <p class="setting-hint">
              {apps.length} apps. Clic droit sur une tuile pour modifier ou supprimer.
            </p>
            <button class="btn-secondary" onclick={() => openAddEditor()}>
              Ajouter une app
            </button>
          </section>

          <section class="setting-group">
            <h3>Raccourci</h3>
            <p class="setting-hint">
              Combinaison globale pour ouvrir/fermer Wisplet. Si elle est déjà prise,
              Wisplet retombe automatiquement sur <code>Ctrl+Alt+Espace</code> puis
              <code>Ctrl+Shift+Espace</code>.
            </p>

            <div class="hotkey-row">
              <span class="setting-label">Raccourci actuel</span>
              <kbd class="hotkey-display" class:conflict={hotkeyConflict}>
                {formatComboLabel(activeHotkey)}
              </kbd>
            </div>

            {#if hotkeyConflict}
              <p class="setting-error">
                Aucun raccourci par défaut n'a pu être enregistré (tous occupés par
                d'autres apps). Définissez-en un personnalisé ci-dessous.
              </p>
            {/if}

            {#if capturingHotkey}
              <div class="capture-box">
                <span class="capture-hint">Appuyez sur la combinaison…</span>
                <kbd class="hotkey-display capturing">
                  {captureBuffer ? formatComboLabel(captureBuffer) : "…"}
                </kbd>
                <button class="btn-secondary" onclick={cancelCapture}>
                  Annuler (Échap)
                </button>
              </div>
            {:else}
              <button class="btn-secondary" onclick={startCapture}>
                Modifier le raccourci
              </button>
            {/if}

            <div class="manual-combo">
              <span class="setting-hint">
                Ou saisir manuellement (ex. <code>F13</code>, <code>Ctrl+Shift+F12</code>) —
                utile pour les touches absentes du clavier physique.
              </span>
              <div class="manual-combo-row">
                <input
                  type="text"
                  placeholder="F13"
                  bind:value={manualCombo}
                  onkeydown={(e) => { e.stopPropagation(); if (e.key === 'Enter' && manualCombo.trim()) applyHotkey(manualCombo.trim()); }}
                />
                <button
                  class="btn-secondary"
                  disabled={!manualCombo.trim()}
                  onclick={() => applyHotkey(manualCombo.trim())}
                >
                  Appliquer
                </button>
              </div>
            </div>

            {#if hotkeyError}
              <p class="setting-error">{hotkeyError}</p>
            {/if}
          </section>

          <section class="setting-group">
            <h3>Organisation</h3>
            <p class="setting-hint">
              Glissez le titre d'un groupe pour réorganiser l'ordre d'affichage.
            </p>
            <button
              class="btn-secondary"
              onclick={resetGroupOrder}
              disabled={settings.groupOrder.length === 0}
            >
              Réinitialiser l'ordre des groupes
            </button>
          </section>

          <div class="drawer-footer">
            <button class="btn-secondary" onclick={resetSettings}>Valeurs par défaut</button>
          </div>
        </div>
      </aside>
    {/if}

    {#if contextMenu}
      <div
        class="ctx-backdrop"
        onclick={() => (contextMenu = null)}
        oncontextmenu={(e) => { e.preventDefault(); contextMenu = null; }}
        role="presentation"
      ></div>
      <div class="ctx-menu" style="left: {contextMenu.x}px; top: {contextMenu.y}px;">
        {#if contextMenu.app}
          <button onclick={() => openEditEditor(contextMenu!.app!)}>Modifier</button>
          <button class="ctx-danger" onclick={() => requestDelete(contextMenu!.app!)}>Supprimer</button>
          <div class="ctx-sep"></div>
        {/if}
        <button onclick={() => openAddEditor(contextMenu!.group)}>Ajouter une app</button>
      </div>
    {/if}

    {#if pendingDelete}
      <div class="modal-backdrop" transition:fade={{ duration: 120 }} role="presentation"></div>
      <div class="modal confirm-modal" transition:scale={{ duration: 140, start: 0.96, easing: quintOut }}>
        <p class="confirm-text">Supprimer <strong>{pendingDelete.title}</strong> ?</p>
        <div class="modal-actions">
          <button class="btn-secondary" onclick={() => (pendingDelete = null)}>Annuler</button>
          <button class="btn-danger" onclick={confirmDelete}>Supprimer</button>
        </div>
      </div>
    {/if}

    {#if editorOpen && editorDraft}
      <div class="modal-backdrop" transition:fade={{ duration: 120 }} role="presentation"></div>
      <div class="modal editor-modal" transition:scale={{ duration: 140, start: 0.96, easing: quintOut }}>
        <div class="modal-header">
          <h2>{editorMode === "add" ? "Ajouter une app" : "Modifier l'app"}</h2>
          <button class="drawer-close" onclick={closeEditor} aria-label="Close">✕</button>
        </div>

        <div class="editor-body">
          <label class="field">
            <span class="field-label">Chemin</span>
            <div class="field-row">
              <input
                type="text"
                bind:value={editorDraft.path}
                placeholder="C:\…\app.exe"
                spellcheck="false"
                autocomplete="off"
                onkeydown={(e) => e.stopPropagation()}
              />
              <button class="btn-secondary" onclick={browsePath}>Parcourir…</button>
            </div>
            <span class="field-hint">Sélectionner un .exe / .lnk pré-remplit l'icône et le titre.</span>
          </label>

          <div class="field">
            <span class="field-label">Icône</span>
            <div class="icon-field">
              <div class="icon-preview">
                {#if editorBusy}
                  <span class="icon-spinner"></span>
                {:else if editorDraft.icon}
                  <img src={convertFileSrc(editorDraft.icon)} alt="" />
                {:else}
                  <div class="icon-fallback">{editorDraft.title.trim()[0] ?? "?"}</div>
                {/if}
              </div>
              <div class="icon-actions">
                <button class="btn-secondary" onclick={browseIcon}>Choisir une icône…</button>
                <button
                  class="btn-secondary"
                  onclick={clearIcon}
                  disabled={!editorDraft.icon}
                >
                  Réinitialiser
                </button>
              </div>
            </div>
          </div>

          <label class="field">
            <span class="field-label">Titre</span>
            <input
              type="text"
              bind:value={editorDraft.title}
              placeholder="Nom de l'app"
              spellcheck="false"
              autocomplete="off"
              onkeydown={(e) => e.stopPropagation()}
            />
          </label>

          <label class="field">
            <span class="field-label">Groupe</span>
            <input
              type="text"
              list="group-list"
              bind:value={editorDraft.group}
              placeholder="Autres"
              spellcheck="false"
              autocomplete="off"
              onkeydown={(e) => e.stopPropagation()}
            />
            <datalist id="group-list">
              {#each groupNames as g (g)}
                <option value={g}></option>
              {/each}
            </datalist>
          </label>

          {#if editorError}
            <p class="setting-error">{editorError}</p>
          {/if}
        </div>

        <div class="modal-actions">
          <button class="btn-secondary" onclick={closeEditor}>Annuler</button>
          <button class="btn-primary" onclick={saveEditor}>
            {editorMode === "add" ? "Ajouter" : "Enregistrer"}
          </button>
        </div>
      </div>
    {/if}
  </main>
{/if}

<style>
  :global(:root) {
    color-scheme: dark;
    font-family: "Segoe UI Variable", "Segoe UI", system-ui, sans-serif;
    --bg-tile: rgba(255, 255, 255, 0.05);
    --bg-tile-hover: rgba(255, 255, 255, 0.1);
    --bg-tile-active: rgba(255, 255, 255, 0.16);
    --fg: #f0f0f2;
    --fg-dim: #a8a8b0;
    --accent: #8ab4ff;
    --accent-rgb: 138, 180, 255;
    --card-bg: rgba(20, 20, 26, 0.55);
    --card-border: rgba(255, 255, 255, 0.06);
  }

  .root {
    height: 100vh;
    width: 100vw;
    padding: 24px 32px;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    color: var(--fg);
    overflow: hidden;
    position: relative;
    opacity: 0;
    transform: scale(0.98);
    pointer-events: none;
    transition: opacity 120ms ease-out, transform 120ms ease-out;
  }
  .root.visible {
    opacity: 1;
    transform: scale(1);
    pointer-events: auto;
    transition: opacity 150ms cubic-bezier(0.22, 1, 0.36, 1), transform 150ms cubic-bezier(0.22, 1, 0.36, 1);
  }
  /* Background is provided entirely by native Win11 windowEffects (Mica/Acrylic).
     No CSS overrides — each mode has its own setEffects() call. */
  /* Overlay sombre additionnel pour mode "overlay" — Svelte applique la classe sur main */
  .root.overlay::before {
    content: "";
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
    pointer-events: none;
    z-index: 0;
  }
  .root > * { position: relative; z-index: 1; }

  .bg-toggle {
    position: fixed;
    top: 14px;
    right: 18px;
    background: rgba(255, 255, 255, 0.08);
    border: 1px solid rgba(255, 255, 255, 0.12);
    color: var(--fg-dim);
    border-radius: 8px;
    padding: 6px 10px;
    font-size: 11px;
    font-family: ui-monospace, "Cascadia Mono", monospace;
    cursor: pointer;
    z-index: 10;
    transition: background 120ms;
  }
  .bg-toggle:hover { background: rgba(255, 255, 255, 0.14); color: var(--fg); }

  .top-bar {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    margin-bottom: 20px;
  }

  .search-wrap {
    position: relative;
    width: 100%;
    max-width: 560px;
    display: flex;
    align-items: center;
  }

  .search-icon {
    position: absolute;
    left: 14px;
    width: 18px;
    height: 18px;
    color: var(--fg-dim);
    pointer-events: none;
  }

  .search-wrap input {
    flex: 1;
    background: rgba(255, 255, 255, 0.06);
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 12px;
    padding: 12px 40px 12px 42px;
    color: var(--fg);
    font-size: 14px;
    outline: none;
    transition: background 120ms, border-color 120ms;
  }
  .search-wrap input:focus {
    background: rgba(255, 255, 255, 0.09);
    border-color: rgba(var(--accent-rgb), 0.4);
  }
  .search-wrap input::placeholder { color: var(--fg-dim); }

  .clear {
    position: absolute;
    right: 12px;
    background: rgba(255, 255, 255, 0.08);
    border: none;
    color: var(--fg-dim);
    width: 22px;
    height: 22px;
    border-radius: 11px;
    cursor: pointer;
    font-size: 11px;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .clear:hover { background: rgba(255, 255, 255, 0.16); color: var(--fg); }

  .error-banner {
    background: rgba(255, 80, 80, 0.18);
    border: 1px solid rgba(255, 80, 80, 0.35);
    color: #ffdbdb;
    padding: 10px 14px;
    border-radius: 10px;
    margin-bottom: 16px;
    font-size: 13px;
    font-family: ui-monospace, "Cascadia Mono", "JetBrains Mono", monospace;
    word-break: break-word;
  }
  .error-banner.empty {
    background: rgba(255, 200, 80, 0.15);
    border-color: rgba(255, 200, 80, 0.35);
    color: #ffe6b0;
  }

  .hotkey-fallback-banner {
    display: flex;
    align-items: center;
    gap: 10px;
    background: rgba(var(--accent-rgb), 0.12);
    border: 1px solid rgba(var(--accent-rgb), 0.3);
    color: #e3ecff;
    padding: 8px 12px;
    border-radius: 10px;
    margin-bottom: 12px;
    font-size: 12.5px;
  }
  .hotkey-fallback-banner span { flex: 1; }
  .hotkey-fallback-banner kbd {
    font-family: ui-monospace, "Cascadia Mono", "JetBrains Mono", monospace;
    background: rgba(255, 255, 255, 0.08);
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 4px;
    padding: 1px 6px;
    font-size: 11.5px;
  }
  .banner-dismiss {
    background: transparent;
    border: none;
    color: inherit;
    opacity: 0.7;
    cursor: pointer;
    font-size: 13px;
    padding: 2px 6px;
    border-radius: 4px;
    transition: opacity 120ms, background 120ms;
  }
  .banner-dismiss:hover { opacity: 1; background: rgba(255, 255, 255, 0.06); }

  .columns {
    flex: 1;
    overflow-y: auto;
    display: grid;
    grid-auto-flow: column;
    grid-auto-columns: 1fr;
    gap: 12px;
    align-content: start;
    align-items: start;
  }

  .column {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .card {
    background: var(--card-bg);
    border: 1px solid var(--card-border);
    border-radius: 14px;
    padding: 14px 12px 12px;
    backdrop-filter: blur(20px);
    position: relative;
    transition: opacity 140ms ease;
  }
  .card.dragging { opacity: 0.4; }
  .card.drop-before::before,
  .card.drop-after::after {
    content: "";
    position: absolute;
    left: 8px;
    right: 8px;
    height: 3px;
    border-radius: 2px;
    background: var(--accent);
    box-shadow: 0 0 8px rgba(var(--accent-rgb), 0.6);
    pointer-events: none;
  }
  .card.drop-before::before { top: -7px; }
  .card.drop-after::after { bottom: -7px; }

  .section-title {
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-dim);
    margin: 0 0 10px 4px;
    display: flex;
    align-items: center;
    gap: 8px;
    user-select: none;
  }
  .section-title.draggable { cursor: grab; }
  .section-title.draggable:active { cursor: grabbing; }
  .section-title .count {
    font-weight: 500;
    font-size: 11px;
    opacity: 0.6;
    background: rgba(255, 255, 255, 0.08);
    padding: 1px 6px;
    border-radius: 8px;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(var(--tile-min, 72px), 1fr));
    gap: 4px;
  }

  .app {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: flex-start;
    padding: 8px 4px 6px;
    background: transparent;
    border: none;
    border-radius: 10px;
    cursor: pointer;
    color: var(--fg);
    transition: background 140ms ease, transform 140ms ease;
    min-height: calc(var(--icon-size, 44px) + 42px);
    /* GPU layer promotion to mitigate Acrylic redraw glitches on hover. */
    will-change: transform, background-color;
    transform: translateZ(0);
    backface-visibility: hidden;
  }
  .app:hover {
    background: var(--bg-tile-hover);
    transform: translate3d(0, -2px, 0);
  }
  .app:active {
    background: var(--bg-tile-active);
    transform: translate3d(0, 0, 0);
  }

  .icon-wrap {
    width: var(--icon-size, 44px);
    height: var(--icon-size, 44px);
    display: flex;
    align-items: center;
    justify-content: center;
    margin-bottom: 6px;
    filter: drop-shadow(0 2px 6px rgba(0, 0, 0, 0.4));
  }
  .icon-wrap img {
    width: 100%;
    height: 100%;
    object-fit: contain;
  }
  .icon-fallback {
    width: 100%;
    height: 100%;
    border-radius: 10px;
    background: linear-gradient(135deg, #4a5168, #2d3245);
    display: flex;
    align-items: center;
    justify-content: center;
    font-weight: 600;
    font-size: calc(var(--icon-size, 44px) * 0.4);
  }

  .label {
    font-size: var(--label-size, 10.5px);
    line-height: 1.25;
    text-align: center;
    color: var(--fg);
    max-width: calc(var(--tile-min, 72px) + 6px);
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    word-break: break-word;
  }

  /* ===== Settings drawer ===== */
  .settings-btn {
    position: absolute;
    right: 0;
    top: 50%;
    transform: translateY(-50%);
    width: 36px;
    height: 36px;
    border-radius: 50%;
    background: transparent;
    border: none;
    color: var(--fg-dim);
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: color 140ms, transform 260ms ease;
  }
  .settings-btn svg { width: 18px; height: 18px; }
  .settings-btn:hover {
    color: var(--fg);
    transform: translateY(-50%) rotate(60deg);
  }
  .settings-btn:active { transform: translateY(-50%) rotate(120deg); }

  .drawer-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.12);
    z-index: 60;
  }

  .drawer {
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    width: 360px;
    background: rgba(18, 18, 22, 0.95);
    backdrop-filter: blur(24px);
    border-left: 1px solid rgba(255, 255, 255, 0.08);
    z-index: 70;
    display: flex;
    flex-direction: column;
    box-shadow: -10px 0 40px rgba(0, 0, 0, 0.4);
  }
  .drawer-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 20px 22px 14px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
  }
  .drawer-header h2 {
    font-size: 16px;
    font-weight: 600;
    margin: 0;
    color: var(--fg);
  }
  .drawer-close {
    background: transparent;
    border: none;
    color: var(--fg-dim);
    cursor: pointer;
    width: 28px;
    height: 28px;
    border-radius: 6px;
    font-size: 12px;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .drawer-close:hover { background: rgba(255, 255, 255, 0.08); color: var(--fg); }

  .drawer-body {
    flex: 1;
    overflow-y: auto;
    padding: 16px 22px 22px;
    display: flex;
    flex-direction: column;
    gap: 24px;
  }
  .drawer-footer {
    margin-top: auto;
    padding-top: 16px;
    border-top: 1px solid rgba(255, 255, 255, 0.06);
  }

  .setting-group h3 {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--fg-dim);
    margin: 0 0 14px;
  }

  .setting-row {
    display: grid;
    grid-template-columns: 1fr auto;
    grid-template-areas: "label value" "input input";
    gap: 4px 8px;
    align-items: center;
    margin-bottom: 18px;
  }
  .setting-label { grid-area: label; font-size: 13px; color: var(--fg); }
  .setting-value { grid-area: value; font-size: 11px; color: var(--fg-dim); font-variant-numeric: tabular-nums; }
  .setting-row input[type="range"] {
    grid-area: input;
    width: 100%;
    accent-color: var(--accent);
    margin-top: 4px;
  }

  .seg {
    grid-area: input;
    display: flex;
    gap: 4px;
    background: rgba(255, 255, 255, 0.04);
    padding: 4px;
    border-radius: 8px;
    margin-top: 6px;
  }
  .seg button {
    flex: 1;
    background: transparent;
    border: none;
    color: var(--fg-dim);
    padding: 6px 8px;
    border-radius: 5px;
    font-size: 11.5px;
    cursor: pointer;
    transition: background 120ms, color 120ms;
  }
  .seg button:hover { color: var(--fg); }
  .seg button.active {
    background: rgba(var(--accent-rgb), 0.18);
    color: var(--accent);
  }

  /* Toggle switch */
  .toggle-row {
    grid-template-columns: 1fr auto;
    grid-template-areas: "label switch";
    align-items: center;
  }
  .toggle-row .setting-label { grid-area: label; }
  .switch {
    grid-area: switch;
    position: relative;
    display: inline-block;
    width: 38px;
    height: 22px;
    cursor: pointer;
  }
  .switch input {
    position: absolute;
    inset: 0;
    opacity: 0;
    margin: 0;
    cursor: pointer;
    z-index: 2;
  }
  .switch-track {
    position: absolute;
    inset: 0;
    background: rgba(255, 255, 255, 0.12);
    border-radius: 11px;
    transition: background 160ms;
  }
  .switch-thumb {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: #d8d8e0;
    transition: transform 200ms cubic-bezier(0.4, 0, 0.2, 1), background 160ms;
  }
  .switch.on .switch-track { background: var(--accent); }
  .switch.on .switch-thumb { transform: translateX(16px); background: #fff; }

  .accent-picker {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .accent-hex {
    font-family: ui-monospace, Consolas, monospace;
    font-size: 11px;
    color: var(--fg-dim);
    font-variant-numeric: tabular-nums;
  }
  .accent-picker input[type="color"] {
    -webkit-appearance: none;
    appearance: none;
    width: 28px;
    height: 22px;
    padding: 0;
    border: 1px solid rgba(255, 255, 255, 0.14);
    border-radius: 6px;
    background: transparent;
    cursor: pointer;
    overflow: hidden;
  }
  .accent-picker input[type="color"]::-webkit-color-swatch-wrapper { padding: 0; }
  .accent-picker input[type="color"]::-webkit-color-swatch {
    border: none;
    border-radius: 4px;
  }
  .accent-reset {
    background: transparent;
    border: 1px solid rgba(255, 255, 255, 0.12);
    color: var(--fg-dim);
    width: 22px;
    height: 22px;
    border-radius: 6px;
    cursor: pointer;
    font-size: 13px;
    line-height: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: background 120ms, color 120ms;
  }
  .accent-reset:hover:not(:disabled) { background: rgba(255, 255, 255, 0.08); color: var(--fg); }
  .accent-reset:disabled { opacity: 0.35; cursor: not-allowed; }

  .btn-secondary {
    background: rgba(255, 255, 255, 0.06);
    border: 1px solid rgba(255, 255, 255, 0.08);
    color: var(--fg);
    padding: 8px 14px;
    border-radius: 8px;
    cursor: pointer;
    font-size: 12.5px;
    transition: background 120ms;
  }
  .btn-secondary:hover:not(:disabled) { background: rgba(255, 255, 255, 0.12); }
  .btn-secondary:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }

  .setting-hint {
    font-size: 12px;
    color: var(--fg-dim);
    margin: 0 0 12px;
    line-height: 1.4;
  }
  .setting-hint code {
    background: rgba(255, 255, 255, 0.08);
    border-radius: 4px;
    padding: 1px 5px;
    font-family: ui-monospace, Consolas, monospace;
    font-size: 11px;
  }

  .setting-error {
    font-size: 12px;
    color: #ff9a9a;
    margin: 8px 0 12px;
    line-height: 1.4;
  }

  .hotkey-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 12px;
  }

  .hotkey-display {
    background: rgba(255, 255, 255, 0.08);
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 6px;
    padding: 4px 10px;
    font-family: ui-monospace, Consolas, monospace;
    font-size: 12px;
    color: var(--fg);
    min-width: 120px;
    text-align: center;
  }
  .hotkey-display.conflict {
    border-color: rgba(255, 154, 154, 0.45);
    color: #ff9a9a;
  }
  .hotkey-display.capturing {
    border-color: var(--accent);
    color: var(--accent);
    animation: pulse 1.2s ease-in-out infinite;
  }
  @keyframes pulse {
    0%, 100% { box-shadow: 0 0 0 0 rgba(var(--accent-rgb), 0.4); }
    50% { box-shadow: 0 0 0 4px rgba(var(--accent-rgb), 0); }
  }

  .capture-box {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 8px;
    padding: 12px;
    background: rgba(255, 255, 255, 0.04);
    border: 1px dashed rgba(var(--accent-rgb), 0.35);
    border-radius: 8px;
  }
  .capture-hint {
    font-size: 12px;
    color: var(--fg-dim);
  }

  .manual-combo {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 10px;
  }
  .manual-combo-row {
    display: flex;
    gap: 8px;
  }
  .manual-combo-row input {
    flex: 1;
    background: rgba(255, 255, 255, 0.06);
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 8px;
    color: var(--fg);
    padding: 7px 10px;
    font-size: 13px;
    font-family: ui-monospace, "Cascadia Mono", "JetBrains Mono", monospace;
    outline: none;
    transition: border-color 120ms, background 120ms;
  }
  .manual-combo-row input:focus {
    border-color: var(--accent);
    background: rgba(255, 255, 255, 0.08);
  }
  .manual-combo-row button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  /* ===== Context menu ===== */
  .ctx-backdrop {
    position: fixed;
    inset: 0;
    z-index: 95;
  }
  .ctx-menu {
    position: fixed;
    z-index: 100;
    min-width: 140px;
    background: rgba(28, 28, 34, 0.98);
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 8px;
    padding: 4px;
    box-shadow: 0 8px 28px rgba(0, 0, 0, 0.5);
    display: flex;
    flex-direction: column;
  }
  .ctx-menu button {
    background: transparent;
    border: none;
    color: var(--fg);
    text-align: left;
    padding: 7px 10px;
    border-radius: 5px;
    font-size: 12.5px;
    cursor: pointer;
    transition: background 100ms;
  }
  .ctx-menu button:hover { background: rgba(255, 255, 255, 0.09); }
  .ctx-menu .ctx-danger { color: #ff9a9a; }
  .ctx-menu .ctx-danger:hover { background: rgba(255, 80, 80, 0.16); }
  .ctx-sep {
    height: 1px;
    background: rgba(255, 255, 255, 0.08);
    margin: 4px 6px;
  }

  /* ===== Modals (editor + delete confirm) ===== */
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    z-index: 80;
  }
  .modal {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    z-index: 90;
    background: rgba(20, 20, 26, 0.98);
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 14px;
    box-shadow: 0 24px 64px rgba(0, 0, 0, 0.55);
    display: flex;
    flex-direction: column;
  }
  .editor-modal { width: 440px; }
  .confirm-modal { width: 320px; padding: 20px; gap: 16px; }

  .modal-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 16px 18px 12px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
  }
  .modal-header h2 {
    font-size: 15px;
    font-weight: 600;
    margin: 0;
    color: var(--fg);
  }

  .editor-body {
    padding: 16px 18px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .field-label {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-dim);
  }
  .field-hint {
    font-size: 11px;
    color: var(--fg-dim);
    opacity: 0.8;
  }
  .field input[type="text"] {
    width: 100%;
    box-sizing: border-box;
    background: rgba(255, 255, 255, 0.06);
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 8px;
    color: var(--fg);
    padding: 8px 10px;
    font-size: 13px;
    outline: none;
    transition: border-color 120ms, background 120ms;
  }
  .field input[type="text"]:focus {
    border-color: var(--accent);
    background: rgba(255, 255, 255, 0.08);
  }
  .field-row {
    display: flex;
    gap: 8px;
  }
  .field-row input { flex: 1; }
  .field-row .btn-secondary { white-space: nowrap; }

  .icon-field {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .icon-preview {
    width: 52px;
    height: 52px;
    flex-shrink: 0;
    border-radius: 10px;
    background: rgba(255, 255, 255, 0.05);
    border: 1px solid rgba(255, 255, 255, 0.08);
    display: flex;
    align-items: center;
    justify-content: center;
    overflow: hidden;
  }
  .icon-preview img {
    width: 80%;
    height: 80%;
    object-fit: contain;
  }
  .icon-preview .icon-fallback {
    width: 80%;
    height: 80%;
    font-size: 20px;
  }
  .icon-spinner {
    width: 18px;
    height: 18px;
    border: 2px solid rgba(255, 255, 255, 0.2);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to { transform: rotate(360deg); }
  }
  .icon-actions {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    padding: 12px 18px 16px;
  }
  .confirm-modal .modal-actions { padding: 0; }
  .confirm-text {
    font-size: 13.5px;
    color: var(--fg);
    margin: 0;
    line-height: 1.4;
  }

  .btn-primary {
    background: var(--accent);
    border: 1px solid var(--accent);
    color: #10131c;
    padding: 8px 16px;
    border-radius: 8px;
    cursor: pointer;
    font-size: 12.5px;
    font-weight: 600;
    transition: filter 120ms;
  }
  .btn-primary:hover { filter: brightness(1.1); }

  .btn-danger {
    background: rgba(255, 80, 80, 0.16);
    border: 1px solid rgba(255, 80, 80, 0.4);
    color: #ff9a9a;
    padding: 8px 16px;
    border-radius: 8px;
    cursor: pointer;
    font-size: 12.5px;
    font-weight: 600;
    transition: background 120ms;
  }
  .btn-danger:hover { background: rgba(255, 80, 80, 0.26); }
</style>
