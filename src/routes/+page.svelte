<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { convertFileSrc } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
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
  };

  const DEFAULT_SETTINGS: Settings = {
    iconSize: 44,
    labelSize: 10.5,
    tileMin: 72,
    bgMode: "mica",
    fullscreen: false,
  };

  function loadSettings(): Settings {
    try {
      const raw = localStorage.getItem("wisplet.settings");
      if (!raw) return { ...DEFAULT_SETTINGS };
      const parsed = JSON.parse(raw);
      // Migrate legacy bgMode values (acrylic/overlay → mica)
      if (parsed.bgMode !== "mica") parsed.bgMode = "mica";
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

  let searchInput = $state<HTMLInputElement | null>(null);

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
    return [...map.entries()]
      .map(([name, apps]) => ({ name, apps: apps.slice().sort((a, b) => a.title.localeCompare(b.title)) }))
      .sort((a, b) => a.name.localeCompare(b.name));
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
    if (e.key === "Escape") {
      if (settingsOpen) settingsOpen = false;
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

  onMount(async () => {
    updateColumnCount();
    window.addEventListener("resize", updateColumnCount);

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
    return () => {
      unlistenIn();
      unlistenOut();
      unlistenFocus();
    };
  });

  function onAnimOutEnd() {
    invoke("hide_window");
  }
</script>

<svelte:window onkeydown={onKey} />

{#if mounted}
  <main
    class="root {settings.bgMode}"
    class:visible
    style="--icon-size: {settings.iconSize}px; --label-size: {settings.labelSize}px; --tile-min: {settings.tileMin}px;"
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

    {#if loadError}
      <div class="error-banner">
        <strong>Erreur load_apps:</strong> {loadError}
      </div>
    {:else if apps.length === 0}
      <div class="error-banner empty">Aucune app trouvée (apps.length === 0)</div>
    {/if}

    <div class="columns">
      {#each columns as col, ci (ci)}
        <div class="column">
          {#each col as section (section.name)}
            <section
              class="card"
              in:fade={{ duration: 220, delay: 40 * ci }}
              animate:flip={{ duration: 300, easing: quintOut }}
            >
              <h3 class="section-title">
                {section.name}
                <span class="count">{section.apps.length}</span>
              </h3>
              <div class="grid">
                {#each section.apps as app (app.id)}
                  <button
                    class="app"
                    onclick={() => launch(app)}
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
          </section>

          <div class="drawer-footer">
            <button class="btn-secondary" onclick={resetSettings}>Valeurs par défaut</button>
          </div>
        </div>
      </aside>
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
    border-color: rgba(138, 180, 255, 0.4);
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
  }

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
  }
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
    background: rgba(138, 180, 255, 0.18);
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
  .btn-secondary:hover { background: rgba(255, 255, 255, 0.12); }
</style>
