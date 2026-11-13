import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";

// ---------- config ----------

const WIN_W = 460; // must match tracker.rs
const HIDE_AFTER_MS = 2500; // retract this long after the cursor leaves
const HIDE_AFTER_DROP_MS = 1300;
const DWELL_MS = 140; // hover this long before it peeks (avoids accidental opens)
const DWELL_FULLSCREEN_MS = 900; // longer hold while a game/video is fullscreen
const VOLUME_PER_NOTCH = 0.03; // mouse wheel: 3% per notch
const VOLUME_PER_PX = 0.0006; // touchpad: per pixel of two-finger travel
const BRIGHT_PER_NOTCH = 5;
const BRIGHT_PER_PX = 0.08;
const MAX_QUICK_APPS = 6;
const REFRESH_AT_REST_MS = 20000; // see setResting()
const SHELF_PER_PAGE = 6;

type State = "idle" | "peek" | "drop" | "open";
const SIZE: Record<State, [number, number]> = {
  idle: [132, 6],
  peek: [312, 42],
  drop: [300, 54],
  open: [404, 128],
};
const TABS = [
  ["player", "Now playing"],
  ["apps", "Apps"],
  ["shelf", "Shelf"],
  ["clipboard", "Clipboard"],
  ["sound", "Sound"],
  ["quick", "Quick"],
] as const;
type TabId = (typeof TABS)[number][0];

interface Media {
  has: boolean; title: string; artist: string; album: string; app: string; playing: boolean;
  canPrev: boolean; canNext: boolean; position: number; duration: number; sampledAt: number;
}
interface Source { id: string; app: string; title: string; playing: boolean; active: boolean }
interface AppView { name: string; target: string; kind: "app" | "url"; icon: string | null }
interface ShelfItem { id: number; path: string; source: string; name: string; size: number; dir: boolean; copied: boolean; icon: string | null }
interface Clip { id: number; kind: "text" | "image" | "files"; text: string | null; thumb: string | null; files: string[]; detail: string }
interface Settings { shelfMode: "link" | "copy"; clipboardSize: number; clipboardPaused: boolean; touchpadInvert: boolean }
interface Boot { fullscreen: boolean; hovered: boolean; settings: Settings; touchpadNatural: boolean; shelf: ShelfItem[]; clipboard: Clip[] }

// ---------- springs ----------

/** Damped spring sampled into a CSS linear() easing. zeta < 1 overshoots a little. */
function spring(zeta: number, samples = 56): string {
  const w = zeta < 1 ? 6.6 / zeta : 9;
  const wd = w * Math.sqrt(Math.max(0, 1 - zeta * zeta));
  const pts: string[] = [];
  for (let i = 0; i <= samples; i++) {
    const t = i / samples;
    const x =
      zeta < 1
        ? 1 - Math.exp(-zeta * w * t) * (Math.cos(wd * t) + ((zeta * w) / wd) * Math.sin(wd * t))
        : 1 - (1 + w * t) * Math.exp(-w * t);
    pts.push(i === samples ? "1" : x.toFixed(4));
  }
  return `linear(${pts.join(", ")})`;
}
const root = document.documentElement.style;
root.setProperty("--spring-open", spring(0.74));
root.setProperty("--spring-close", spring(1));

// ---------- dom helpers ----------

const $ = <T extends Element = HTMLElement>(s: string, el: ParentNode = document) => el.querySelector(s) as T;
const $$ = <T extends Element = HTMLElement>(s: string, el: ParentNode = document) =>
  Array.from(el.querySelectorAll(s)) as T[];
const setText = (k: string, v: string) =>
  $$(`[data-bind="${k}"]`).forEach((el) => el.textContent !== v && (el.textContent = v));

function h<K extends keyof HTMLElementTagNameMap>(tag: K, cls = "", html = ""): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  if (cls) el.className = cls;
  if (html) el.innerHTML = html;
  return el;
}
const icon = (id: string) => `<svg><use href="#i-${id}"/></svg>`;
const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]!);

const BRAND: Record<string, string> = {
  youtube: "#ff0033", netflix: "#e50914", twitch: "#9146ff", reddit: "#ff4500", github: "#24292f",
  gmail: "#ea4335", google: "#4285f4", chatgpt: "#10a37f", x: "#000", twitter: "#1d9bf0",
  instagram: "#d62976", facebook: "#1877f2", whatsapp: "#25d366", discord: "#5865f2", notion: "#191919",
};

/** A letter tile with a brand or stable per-name colour, for websites and icon-less items. */
function mono(name: string): HTMLElement {
  let hash = 0;
  for (const ch of name) hash = (hash * 31 + ch.charCodeAt(0)) | 0;
  const el = h("span", "ic mono", esc((name.trim()[0] || "?").toUpperCase()));
  el.style.setProperty("--mono", BRAND[name.trim().toLowerCase()] ?? `hsl(${Math.abs(hash) % 360} 52% 40%)`);
  return el;
}
function iconBox(src: string | null, name: string): HTMLElement {
  if (!src) return mono(name);
  const box = h("span", "ic");
  const img = h("img");
  img.src = src;
  img.alt = "";
  box.append(img);
  return box;
}

const island = $("#island");

// ---------- island state ----------

let state: State = "idle";
let hovered = false;
let fullscreen = false;
let dwellTimer = 0;
let hideTimer = 0;
let resting = false;
let dirty = false; // has the panel been shown since this page loaded?
let refreshTimer = 0;
let settings: Settings = { shelfMode: "link", clipboardSize: 5, clipboardPaused: false, touchpadInvert: false };
let touchpadNatural = true;

/** Low-memory mode for the web view while Perch is tucked away. */
function setResting(on: boolean) {
  if (on === resting) return;
  resting = on;
  // At rest only the tiny pill is visible: take every panel out of the page so its
  // drawing memory and decoded images can be freed, then ask WebView2 to trim.
  document.body.classList.toggle("rest", on);
  invoke("set_resting", { resting: on });
  // The page process keeps memory it grew while panels were open. Once Perch has been
  // tucked away for a while, reload the page: all real state lives in the Rust core,
  // so this just returns memory to its fresh-start level.
  clearTimeout(refreshTimer);
  if (on && dirty) {
    refreshTimer = window.setTimeout(() => {
      if (resting && state === "idle" && !hovered) location.reload();
    }, REFRESH_AT_REST_MS);
  }
}

function setState(next: State) {
  if (next === state) return;
  const grow = SIZE[next][1] > SIZE[state][1];
  island.style.setProperty("--ease", grow ? "var(--spring-open)" : "var(--spring-close)");
  island.style.setProperty("--dur", grow ? "560ms" : "380ms");
  const prev = state;
  if (next === "open" || next === "drop") dirty = true;
  island.dataset.state = next;
  state = next;
  reportHitRect();
  if (next === "open") startTicker();
  else stopTicker();
  if (prev === "open") closeTransient();
  if (next !== "drop") island.classList.remove("receiving", "dropped");
  if (next === "idle") {
    setTimeout(() => {
      if (state !== "idle") return;
      setTab(0, true);
      setResting(true);
    }, 600);
  }
  updateGlass();
  requestAnimationFrame(updateMarquees);
}

/** Tell Rust where the clickable area is so the rest of the window stays click-through. */
function reportHitRect() {
  const [w, hh] = SIZE[state];
  const hw = state === "idle" ? 210 : w + 2 * 14; // include the ear fillets
  const ht = state === "idle" ? 8 : hh + 6;
  invoke("set_hit_rect", { x: (WIN_W - hw) / 2, y: 0, w: hw, h: ht });
}

function scheduleHide(ms = HIDE_AFTER_MS) {
  clearTimeout(hideTimer);
  hideTimer = window.setTimeout(() => {
    if (hovered || searching) return;
    setState("idle");
  }, ms);
}

listen<boolean>("hover", ({ payload }) => {
  hovered = payload;
  if (hovered) {
    clearTimeout(hideTimer);
    setResting(false);
    volKnown = false;
    briKnown = false;
    if (state === "idle") {
      clearTimeout(dwellTimer);
      dwellTimer = window.setTimeout(
        () => hovered && state === "idle" && setState("peek"),
        fullscreen ? DWELL_FULLSCREEN_MS : DWELL_MS,
      );
    }
  } else {
    clearTimeout(dwellTimer);
    if (state !== "idle") scheduleHide(state === "drop" ? HIDE_AFTER_DROP_MS : HIDE_AFTER_MS);
  }
});
listen<boolean>("fullscreen", ({ payload }) => (fullscreen = payload));

// Click anywhere on the peek (or a just-filled drop) that isn't a button -> open the panel.
island.addEventListener("click", (e) => {
  if ((e.target as Element).closest("button")) return;
  if (state === "peek") setState("open");
  else if (state === "drop") {
    setTab(TABS.findIndex(([id]) => id === "shelf"), true);
    setState("open");
  }
});
document.addEventListener("contextmenu", (e) => e.preventDefault());

// ---------- tabs + header ----------

let tab = 0;
const lines = $("nav.lines");
TABS.forEach(([, label], i) => {
  const b = h("button");
  b.setAttribute("aria-label", label);
  b.addEventListener("click", () => setTab(i));
  lines.append(b);
});
const tabEls = TABS.map(([id]) => $(`.tab[data-tab="${id}"]`));
const tabLabel = $(".tab-label");
const tabId = (): TabId => TABS[tab][0];

function setTab(i: number, instant = false) {
  i = (i + TABS.length) % TABS.length;
  if (i !== tab) closeTransient();
  tabEls.forEach((el, j) => {
    el.classList.toggle("on", j === i);
    el.classList.toggle("before", j < i);
  });
  $$("button", lines).forEach((b, j) => b.classList.toggle("on", j === i));
  if (i !== tab && !instant) {
    tabLabel.classList.add("swap");
    setTimeout(() => {
      tabLabel.textContent = TABS[i][1];
      tabLabel.classList.remove("swap");
    }, 140);
  } else tabLabel.textContent = TABS[i][1];
  tab = i;
  renderHeader();
  updateGlass();
}

/** Each tab gets its own little set of header chips on the right. */
function renderHeader() {
  const id = tabId();
  $$(".hdr .chip").forEach((c) => {
    let show = c.dataset.for === id;
    if (c.classList.contains("src")) show = show && media.has;
    if (c.dataset.hdr === "clear-shelf") show = show && shelf.length > 0;
    if (c.dataset.hdr === "clear-clip") show = show && clips.length > 0;
    if (c.dataset.hdr === "edit") show = show && quick.length > 0;
    c.classList.toggle("show", show);
  });
  const mode = $('[data-hdr="mode"]');
  mode.textContent = settings.shelfMode === "copy" ? "Copies" : "Linked";
  mode.title =
    settings.shelfMode === "copy"
      ? "New files are copied onto the shelf (originals untouched). Click to keep links instead."
      : "The shelf links to your original files. Click to park copies instead.";
  const pause = $('[data-hdr="pause"]');
  pause.textContent = settings.clipboardPaused ? "Paused" : "Pause";
  pause.classList.toggle("on", settings.clipboardPaused);
  $('[data-hdr="edit"]').textContent = editing ? "Done" : "Edit";
}

/** Close sheets/search/edit mode (on tab change or when the panel closes). */
function closeTransient() {
  $$(".sheet").forEach((s) => s.classList.remove("show"));
  $(".src").classList.remove("open");
  if (searching) exitSearch();
  if (editing) setEditing(false);
}

function updateGlass() {
  island.classList.toggle("glassy", state === "open" && tabId() === "player" && hasArt);
}

$(".hdr").addEventListener("click", async (e) => {
  const c = (e.target as Element).closest<HTMLElement>(".chip");
  if (!c) return;
  e.stopPropagation();
  if (c.classList.contains("src")) return toggleSources();
  switch (c.dataset.hdr) {
    case "edit":
      return setEditing(!editing);
    case "mode":
      return saveSettings({ shelfMode: settings.shelfMode === "copy" ? "link" : "copy" });
    case "clear-shelf":
      return invoke("shelf_clear");
    case "pause":
      return saveSettings({ clipboardPaused: !settings.clipboardPaused });
    case "clear-clip":
      return invoke("clip_clear");
  }
});

async function saveSettings(patch: Partial<Settings>) {
  settings = await invoke<Settings>("settings_patch", { patch });
  renderHeader();
}

// ---------- scroll: volume, brightness, tab swipes ----------
//
// Mouse wheel: roll up = louder.  Touchpad: two fingers up = louder, whatever the
// Windows scrolling direction is.  Ctrl + scroll (or hold right-click + scroll) = brightness.

let lastTouchpadAt = -1e9;
function fromTouchpad(e: WheelEvent): boolean {
  const wdy = (e as unknown as { wheelDeltaY: number }).wheelDeltaY;
  const wdx = (e as unknown as { wheelDeltaX: number }).wheelDeltaX;
  // Mouse wheels send whole notches (multiples of 120) on one axis.
  const notch = e.deltaMode === 0 && wdx === 0 && wdy !== 0 && Math.abs(wdy) % 120 === 0;
  const now = performance.now();
  if (!notch) lastTouchpadAt = now;
  return now - lastTouchpadAt < 350;
}

let vol = 0.5;
let volMuted = false;
let volKnown = false;
let volLoading: Promise<void> | null = null;
let briLoading: Promise<void> | null = null;
let volBusy = false;
let volDirty = false;
let bri: number | null = null;
let briKnown = false;
let hudTimer = 0;
let swipeAcc = 0;
let swipeLock = 0;
let rightScrolled = false;

function pushVolume() {
  if (volBusy) {
    volDirty = true;
    return;
  }
  volBusy = true;
  invoke<{ level: number; muted: boolean }>("volume_set", { level: vol })
    .then((v) => (volMuted = v.muted))
    .finally(() => {
      volBusy = false;
      if (volDirty) {
        volDirty = false;
        pushVolume();
      }
    });
}

function showHud(kind: "vol" | "bri", level: number | null) {
  island.classList.add("vhud");
  island.classList.toggle("bright", kind === "bri");
  island.style.setProperty("--v", String(level ?? 0));
  const text =
    kind === "vol"
      ? volMuted && vol === 0 ? "Muted" : `Volume ${Math.round(vol * 100)}%`
      : level === null ? "Brightness can't be changed on this display" : `Brightness ${Math.round(level * 100)}%`;
  setText("peekSub", text);
  clearTimeout(hudTimer);
  hudTimer = window.setTimeout(() => {
    island.classList.remove("vhud", "bright");
    render();
  }, 1100);
}

document.addEventListener(
  "wheel",
  async (e) => {
    const brightMode = e.ctrlKey || (e.buttons & 2) !== 0;
    if (e.ctrlKey) e.preventDefault(); // no page zoom / pinch zoom
    if (e.buttons & 2) rightScrolled = true;

    const touch = fromTouchpad(e);
    const unit = e.deltaMode === 1 ? 33 : 1;
    const dx = e.deltaX * unit;
    const dy = e.deltaY * unit;

    // Sideways two-finger swipe switches tabs.
    if (!brightMode && Math.abs(dx) > Math.abs(dy) * 1.5) {
      if (state !== "open" || !touch || performance.now() < swipeLock || searching) return;
      swipeAcc += dx;
      if (Math.abs(swipeAcc) > 70) {
        setTab(tab + Math.sign(swipeAcc));
        swipeAcc = 0;
        swipeLock = performance.now() + 420;
      }
      return;
    }
    if (dy === 0) return;

    // "up" is positive: wheel rolled away from you, or fingers moving up.
    let up: number;
    if (touch) {
      const fingersUp = touchpadNatural !== settings.touchpadInvert ? dy : -dy;
      up = fingersUp;
    } else {
      // Count real notches (120 units each), independent of the "lines per scroll" setting.
      const wdy = (e as unknown as { wheelDeltaY: number }).wheelDeltaY;
      up = wdy ? wdy / 120 : -dy / 100;
    }

    if (state === "idle") {
      clearTimeout(dwellTimer);
      setState("peek");
    }

    if (brightMode) {
      // Every notch waits for the same read, so none get dropped while it's in flight.
      if (!briKnown) {
        briLoading ??= invoke<number | null>("brightness_get").then((v) => {
          bri = v;
          briKnown = true;
          briLoading = null;
        });
        await briLoading;
      }
      if (bri === null) return showHud("bri", null);
      bri = Math.min(100, Math.max(0, bri + (touch ? up * BRIGHT_PER_PX : up * BRIGHT_PER_NOTCH)));
      invoke("brightness_set", { level: Math.round(bri) });
      return showHud("bri", bri / 100);
    }

    if (!volKnown) {
      volLoading ??= invoke<{ level: number; muted: boolean }>("volume_get").then((v) => {
        vol = v.level;
        volMuted = v.muted;
        volKnown = true;
        volLoading = null;
      });
      await volLoading;
    }
    vol = Math.min(1, Math.max(0, vol + (touch ? up * VOLUME_PER_PX : up * VOLUME_PER_NOTCH)));
    volMuted = false;
    pushVolume();
    showHud("vol", vol);
  },
  { passive: false },
);
document.addEventListener("pointerup", (e) => {
  if (e.button === 2) setTimeout(() => (rightScrolled = false), 0);
});

// ---------- media ----------

let media: Media = {
  has: false, title: "", artist: "", album: "", app: "", playing: false,
  canPrev: false, canNext: false, position: 0, duration: 0, sampledAt: 0,
};
let sources: Source[] = [];
let hasArt = false;

$$("[data-act]").forEach((b) =>
  b.addEventListener("click", (e) => {
    e.stopPropagation();
    const act = b.dataset.act!;
    if (act === "toggle" && media.has) {
      // optimistic: flip the icon now, Windows confirms a moment later
      media = { ...media, playing: !media.playing, position: currentPos(), sampledAt: Date.now() };
      render();
    }
    invoke("media_cmd", { action: act });
  }),
);

function currentPos(): number {
  if (!media.has || media.duration <= 0) return 0;
  const p = media.position + (media.playing ? (Date.now() - media.sampledAt) / 1000 : 0);
  return Math.min(Math.max(p, 0), media.duration);
}

const fmt = (s: number) => {
  s = Math.floor(s);
  const hr = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return hr ? `${hr}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
};

function render() {
  const m = media;
  setText("title", m.has ? m.title || "Unknown title" : "Nothing playing");
  setText("artist", m.has ? m.artist || m.app : "Play something in Spotify, YouTube, anywhere");
  if (!island.classList.contains("vhud")) setText("peekSub", m.has ? m.artist || m.app : "Hover here any time");
  setText("app", m.app);
  $(".src").classList.toggle("live", m.playing);

  $$<SVGUseElement>(".pp use").forEach((u) => u.setAttribute("href", m.playing ? "#i-pause" : "#i-play"));
  $$<HTMLButtonElement>('[data-act="prev"]').forEach((b) => (b.disabled = !m.canPrev));
  $$<HTMLButtonElement>('[data-act="next"]').forEach((b) => (b.disabled = !m.canNext));
  $$<HTMLButtonElement>('[data-act="toggle"]').forEach((b) => (b.disabled = !m.has));
  $(".progress").classList.toggle("off", !m.has || m.duration <= 0);
  renderHeader();
  tick();
  requestAnimationFrame(updateMarquees);
}

listen<Media>("media", ({ payload }) => {
  media = payload;
  render();
});

// cover art (+ accent colour pulled from it) also feeds the glass backdrop
const artBoxes = $$(".art");
const artImgs = [...$$<HTMLImageElement>(".art img"), $<HTMLImageElement>(".glass img")];
listen<string | null>("media-art", ({ payload }) => {
  hasArt = !!payload;
  artBoxes.forEach((a) => a.classList.toggle("has", hasArt));
  updateGlass();
  if (!payload) {
    root.setProperty("--accent", "245, 245, 247");
    return;
  }
  artImgs.forEach((img) => (img.src = payload));
  artImgs[0].decode().then(() => root.setProperty("--accent", accentOf(artImgs[0])), () => {});
});

/** Pick a lively colour from the art (saturation-weighted average), bright enough for black. */
function accentOf(img: HTMLImageElement): string {
  const c = document.createElement("canvas");
  c.width = c.height = 12;
  const g = c.getContext("2d", { willReadFrequently: true })!;
  g.drawImage(img, 0, 0, 12, 12);
  const d = g.getImageData(0, 0, 12, 12).data;
  let r = 0, gg = 0, b = 0, wsum = 0;
  for (let i = 0; i < d.length; i += 4) {
    const max = Math.max(d[i], d[i + 1], d[i + 2]);
    const min = Math.min(d[i], d[i + 1], d[i + 2]);
    const w = ((max - min) / 255) ** 2 * (max / 255) + 0.002;
    r += d[i] * w; gg += d[i + 1] * w; b += d[i + 2] * w; wsum += w;
  }
  let rgb = [r / wsum, gg / wsum, b / wsum];
  const lum = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
  if (lum < 120) {
    const k = 120 / Math.max(lum, 1);
    rgb = rgb.map((v) => Math.min(255, v * k + 18));
  }
  return rgb.map((v) => Math.round(v)).join(", ");
}

// Which app to control, when more than one is playing or paused.
const srcChip = $(".src");
const sourcesSheet = $(".sources");
listen<Source[]>("media-sources", ({ payload }) => {
  sources = payload;
  srcChip.classList.toggle("multi", sources.length > 1);
  if (sources.length < 2) sourcesSheet.classList.remove("show");
  if (sourcesSheet.classList.contains("show")) renderSources();
});
function toggleSources() {
  const show = !sourcesSheet.classList.contains("show");
  if (show) renderSources();
  sourcesSheet.classList.toggle("show", show);
  srcChip.classList.toggle("open", show);
}
function renderSources() {
  sourcesSheet.replaceChildren(
    ...sources.slice(0, 3).map((s) => {
      const b = h(
        "button",
        `source${s.active ? " active" : ""}${s.playing ? " playing" : ""}`,
        `<b><i></i>${esc(s.app)}</b><span>${esc(s.title || (s.playing ? "Playing" : "Paused"))}</span>`,
      );
      b.addEventListener("click", (e) => {
        e.stopPropagation();
        invoke("media_select", { id: s.id });
        toggleSources();
      });
      return b;
    }),
  );
}

// progress bar: extrapolated locally, only ticking while the panel is open
let ticker = 0;
const bar = $(".bar");
function tick() {
  const p = currentPos();
  bar.style.setProperty("--p", media.duration > 0 ? String(p / media.duration) : "0");
  setText("pos", fmt(p));
  setText("dur", fmt(media.duration));
}
function startTicker() {
  stopTicker();
  tick();
  ticker = window.setInterval(tick, 250);
}
function stopTicker() {
  clearInterval(ticker);
  ticker = 0;
}
$(".progress").addEventListener("click", (e) => {
  e.stopPropagation();
  if (media.duration <= 0) return;
  const r = bar.getBoundingClientRect();
  const secs = Math.min(Math.max((e.clientX - r.left) / r.width, 0), 1) * media.duration;
  media = { ...media, position: secs, sampledAt: Date.now() };
  tick();
  invoke("media_cmd", { action: "seek", value: secs });
});

// long titles scroll gently back and forth instead of being cut off
function updateMarquees() {
  $$(".title").forEach((t) => {
    const span = t.firstElementChild as HTMLElement;
    const visible = t.closest(".layer")?.matches(
      "#island[data-state=peek] .peek, #island[data-state=open] .panel, #island[data-state=drop] .drop",
    );
    const over = span.scrollWidth - t.clientWidth;
    const on = !!visible && over > 2;
    t.classList.toggle("marq", on);
    t.classList.toggle("fade", over > 2);
    if (on) {
      t.style.setProperty("--shift", `${-over - 8}px`);
      t.style.setProperty("--marq-dur", `${Math.max(4, over / 22)}s`);
    }
  });
}

// ---------- apps ----------

let quick: AppView[] = [];
let editing = false;
let searching = false;
let results: AppView[] = [];
let sel = 0;
const appsTab = $('.tab[data-tab="apps"]');
const appsRow = $(".apps-row");
const resultsRow = $(".results");
const searchInput = $<HTMLInputElement>(".search input");

function renderApps() {
  const tiles = quick.map((a, i) => {
    const t = h("button", "tile");
    t.title = a.kind === "url" ? a.target : a.name;
    t.append(iconBox(a.icon, a.name), h("span", "nm", esc(a.name)));
    const rm = h("span", "rm", icon("x"));
    rm.addEventListener("click", (e) => {
      e.stopPropagation();
      saveQuick(quick.filter((_, j) => j !== i));
    });
    t.append(rm);
    t.addEventListener("click", () => {
      if (editing) return;
      invoke("quick_launch", { target: a.target, kind: a.kind });
      setTimeout(() => setState("idle"), 180);
    });
    return t;
  });
  if (quick.length < MAX_QUICK_APPS) {
    const add = h("button", "tile adder");
    add.append(h("span", "ic add", icon("plus")), h("span", "nm", "Add"));
    add.addEventListener("click", enterSearch);
    tiles.push(add);
  }
  appsRow.replaceChildren(...tiles);
  renderHeader();
}

async function saveQuick(list: AppView[]) {
  quick = await invoke<AppView[]>("quick_apps_save", {
    apps: list.map(({ name, target, kind }) => ({ name, target, kind })),
  });
  if (!quick.length) setEditing(false);
  renderApps();
}

function setEditing(on: boolean) {
  editing = on;
  appsTab.classList.toggle("editing", on);
  renderHeader();
}

function enterSearch() {
  setEditing(false);
  searching = true;
  appsTab.classList.add("searching");
  searchInput.value = "";
  results = [];
  renderResults();
  invoke("set_typing", { on: true }).then(() => searchInput.focus());
}

function exitSearch() {
  searching = false;
  appsTab.classList.remove("searching");
  searchInput.blur();
  invoke("set_typing", { on: false });
  if (!hovered && state !== "idle") scheduleHide();
}

function renderResults() {
  if (!results.length) {
    resultsRow.replaceChildren(
      h("span", "hint", searchInput.value.trim() ? "No matches" : "Start typing. Enter adds, Esc goes back."),
    );
    return;
  }
  resultsRow.replaceChildren(
    ...results.map((a, i) => {
      const t = h("button", `tile${i === sel ? " sel" : ""}`);
      t.append(iconBox(a.icon, a.name), h("span", "nm", esc(a.name)));
      t.addEventListener("click", (e) => {
        e.stopPropagation();
        addQuick(a);
      });
      return t;
    }),
  );
}

function addQuick(a: AppView) {
  exitSearch();
  if (quick.some((q) => q.target === a.target)) return;
  saveQuick([...quick, a].slice(0, MAX_QUICK_APPS));
}

let searchSeq = 0;
let searchTimer = 0;
searchInput.addEventListener("input", () => {
  clearTimeout(searchTimer);
  searchTimer = window.setTimeout(async () => {
    const seq = ++searchSeq;
    const r = await invoke<AppView[]>("apps_search", { query: searchInput.value });
    if (seq !== searchSeq) return;
    results = r;
    sel = 0;
    renderResults();
  }, 110);
});
searchInput.addEventListener("keydown", (e) => {
  if (e.key === "Escape") exitSearch();
  else if (e.key === "Enter" && results[sel]) addQuick(results[sel]);
  else if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
    if (!results.length) return;
    e.preventDefault();
    sel = (sel + (e.key === "ArrowRight" ? 1 : -1) + results.length) % results.length;
    renderResults();
  }
});

// ---------- shelf ----------

let shelf: ShelfItem[] = [];
let shelfPage = 0;
let fresh = new Set<number>();
let draggingOut = false;
const shelfRow = $(".shelf-row");
const actionsSheet = $(".actions");

listen<ShelfItem[]>("shelf", ({ payload }) => {
  const known = new Set(shelf.map((i) => i.id));
  fresh = new Set(payload.filter((i) => !known.has(i.id)).map((i) => i.id));
  shelf = payload;
  renderShelf();
});
listen("drag-out-end", () => setTimeout(() => (draggingOut = false), 150));

function renderShelf() {
  const pages = Math.max(1, Math.ceil(shelf.length / (SHELF_PER_PAGE - 1)));
  if (shelf.length <= SHELF_PER_PAGE) shelfPage = 0;
  shelfPage = Math.min(shelfPage, pages - 1);
  const paged = shelf.length > SHELF_PER_PAGE;
  const per = paged ? SHELF_PER_PAGE - 1 : SHELF_PER_PAGE;
  const view = shelf.slice(shelfPage * per, shelfPage * per + per);

  const tiles: HTMLElement[] = view.map((it) => {
    const t = h("button", `tile${fresh.has(it.id) ? " fresh" : ""}`);
    t.title = it.copied ? `${it.name} (copy)` : it.source;
    t.append(iconBox(it.icon, it.name), h("span", "nm", esc(it.name)));
    const rm = h("span", "rm", icon("x"));
    rm.title = "Remove from shelf";
    rm.addEventListener("click", (e) => {
      e.stopPropagation();
      invoke("shelf_action", { id: it.id, action: "remove" });
    });
    t.append(rm);

    // click = open, drag = real file drag out to anywhere, right-click = more actions
    let start: { x: number; y: number } | null = null;
    t.addEventListener("pointerdown", (e) => {
      if (e.button === 0) start = { x: e.clientX, y: e.clientY };
    });
    t.addEventListener("pointermove", (e) => {
      if (!start || !(e.buttons & 1)) return;
      if (Math.hypot(e.clientX - start.x, e.clientY - start.y) > 5) {
        start = null;
        draggingOut = true;
        invoke("shelf_drag", { id: it.id });
      }
    });
    t.addEventListener("click", (e) => {
      e.stopPropagation();
      if (!start) return;
      start = null;
      invoke("shelf_action", { id: it.id, action: "open" });
    });
    t.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      if (!rightScrolled) showActions(it);
    });
    return t;
  });
  if (paged) {
    const more = h("button", "tile more");
    const left = shelf.length - (shelfPage + 1) * per;
    more.append(h("span", "ic", left > 0 ? `+${left}` : "1/" + pages), h("span", "nm", left > 0 ? "More" : "Back"));
    more.addEventListener("click", (e) => {
      e.stopPropagation();
      shelfPage = left > 0 ? shelfPage + 1 : 0;
      fresh.clear();
      renderShelf();
    });
    tiles.push(more);
  }
  shelfRow.replaceChildren(...tiles);
  $('.tab[data-tab="shelf"] .empty-note').classList.toggle("show", shelf.length === 0);
  renderHeader();
}

function showActions(it: ShelfItem) {
  const acts: [string, string, string, boolean?][] = [
    ["open", "open", "Open"],
    ["reveal", "folder", "Show"],
    ["copy", "copy", "Copy"],
    ["remove", "minus", "Remove"],
  ];
  if (!it.copied) acts.push(["trash", "trash", "Recycle", true]);
  actionsSheet.replaceChildren(
    ...acts.map(([action, ic, label, danger]) => {
      const b = h("button", `act${danger ? " danger" : ""}`, `${icon(ic)}<span>${label}</span>`);
      b.title =
        action === "trash" ? `Move "${it.name}" to the Recycle Bin`
        : action === "remove" ? "Take it off the shelf (the file stays where it is)"
        : action === "copy" ? "Copy the file, then paste it anywhere"
        : "";
      b.addEventListener("click", (e) => {
        e.stopPropagation();
        invoke("shelf_action", { id: it.id, action });
        actionsSheet.classList.remove("show");
      });
      return b;
    }),
  );
  actionsSheet.classList.add("show");
}
actionsSheet.addEventListener("click", (e) => {
  if (e.target === actionsSheet) actionsSheet.classList.remove("show");
});

// Files dragged onto the island land on the shelf.
let dropDone = false;
getCurrentWebview().onDragDropEvent(({ payload: p }) => {
  if (draggingOut) return; // our own tile being dragged out
  if (p.type === "enter") {
    dropDone = false;
    clearTimeout(dwellTimer);
    clearTimeout(hideTimer);
    const n = p.paths.length;
    setText("dropTitle", "Drop to keep on Shelf");
    setText("dropSub", n > 1 ? `${n} items` : p.paths[0]?.split(/[\\/]/).pop() ?? "");
    setState("drop");
    island.classList.add("receiving");
    island.classList.remove("dropped");
    requestAnimationFrame(updateMarquees);
  } else if (p.type === "leave") {
    island.classList.remove("receiving");
    if (!dropDone && state === "drop") setState(hovered ? "peek" : "idle");
  } else if (p.type === "drop") {
    dropDone = true;
    island.classList.remove("receiving");
    invoke<number>("shelf_add", { paths: p.paths }).then((n) => {
      island.classList.add("dropped");
      setText("dropTitle", n ? `Added ${n} ${n === 1 ? "item" : "items"} to Shelf` : "Already on your Shelf");
      setText("dropSub", "Click to open the Shelf");
      if (!hovered) scheduleHide(HIDE_AFTER_DROP_MS);
    });
  }
});

// ---------- clipboard ----------

let clips: Clip[] = [];
const clipRow = $(".clip-row");
listen<Clip[]>("clipboard", ({ payload }) => {
  clips = payload;
  renderClips();
});

function renderClips() {
  clipRow.replaceChildren(
    ...clips.map((c) => {
      const card = h("button", "card");
      let body: HTMLElement;
      if (c.kind === "image" && c.thumb) {
        body = h("div", "body pic");
        const img = h("img");
        img.src = c.thumb;
        body.append(img);
      } else if (c.kind === "files") {
        body = h("div", "body file");
        if (c.thumb) {
          const img = h("img");
          img.src = c.thumb;
          body.append(img);
        }
        body.append(h("span", "", esc(c.files[0] ?? "")));
      } else {
        body = h("div", "body", esc(c.text ?? ""));
      }
      card.title = c.kind === "text" ? (c.text ?? "") : c.kind === "files" ? c.files.join("\n") : "Image";
      card.append(body, h("div", "foot", esc(c.detail)), h("div", "done", icon("check")));
      const rm = h("span", "rm", icon("x"));
      rm.addEventListener("click", (e) => {
        e.stopPropagation();
        invoke("clip_remove", { id: c.id });
      });
      card.append(rm);
      card.addEventListener("click", async (e) => {
        e.stopPropagation();
        if (await invoke<boolean>("clip_copy", { id: c.id })) {
          card.classList.add("copied");
          setTimeout(() => card.classList.remove("copied"), 900);
        }
      });
      return card;
    }),
  );
  $('.tab[data-tab="clipboard"] .empty-note').classList.toggle("show", clips.length === 0);
  renderHeader();
}

// ---------- boot ----------

setTab(0, true);
render();
reportHitRect();
invoke<Boot>("ui_ready").then((b) => {
  fullscreen = b.fullscreen;
  hovered = b.hovered;
  settings = b.settings;
  touchpadNatural = b.touchpadNatural;
  shelf = b.shelf;
  clips = b.clipboard;
  renderShelf();
  renderClips();
  renderHeader();
});
invoke<AppView[]>("quick_apps").then((a) => {
  quick = a;
  renderApps();
});
setTimeout(() => state === "idle" && setResting(true), 3000);
