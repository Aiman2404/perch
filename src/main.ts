import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// ---------- config ----------

const WIN_W = 460; // must match tracker.rs
const HIDE_AFTER_MS = 2500; // retract this long after the cursor leaves
const DWELL_MS = 140; // hover this long before it peeks (avoids accidental opens)
const DWELL_FULLSCREEN_MS = 900; // longer hold while a game/video is fullscreen
const VOLUME_PER_NOTCH = 0.03; // 3% per mouse-wheel notch

type State = "idle" | "peek" | "open";
const SIZE: Record<State, [number, number]> = {
  idle: [132, 6],
  peek: [312, 42],
  open: [404, 128],
};
const TABS = [
  ["player", "Now playing"],
  ["shelf", "Shelf"],
  ["clipboard", "Clipboard"],
  ["sound", "Sound"],
  ["quick", "Quick"],
] as const;

interface Media {
  has: boolean;
  title: string;
  artist: string;
  album: string;
  app: string;
  playing: boolean;
  canPrev: boolean;
  canNext: boolean;
  position: number;
  duration: number;
  sampledAt: number;
}

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

// ---------- dom ----------

const $ = <T extends Element = HTMLElement>(s: string, el: ParentNode = document) => el.querySelector(s) as T;
const $$ = <T extends Element = HTMLElement>(s: string, el: ParentNode = document) =>
  Array.from(el.querySelectorAll(s)) as T[];

const island = $("#island");
const binds = (k: string) => $$(`[data-bind="${k}"]`);
const setText = (k: string, v: string) => binds(k).forEach((el) => el.textContent !== v && (el.textContent = v));

// ---------- island state ----------

let state: State = "idle";
let hovered = false;
let fullscreen = false;
let dwellTimer = 0;
let hideTimer = 0;

function setState(next: State) {
  if (next === state) return;
  const grow = SIZE[next][1] > SIZE[state][1];
  island.style.setProperty("--ease", grow ? "var(--spring-open)" : "var(--spring-close)");
  island.style.setProperty("--dur", grow ? "560ms" : "380ms");
  island.dataset.state = next;
  state = next;
  reportHitRect();
  if (next === "open") startTicker();
  else stopTicker();
  if (next === "idle") {
    setTimeout(() => state === "idle" && setTab(0, true), 600);
  }
  requestAnimationFrame(updateMarquees);
}

/** Tell Rust where the clickable area is so the rest of the window stays click-through. */
function reportHitRect() {
  const [w, h] = SIZE[state];
  const hw = state === "idle" ? 210 : w + 2 * 14; // include the ear fillets
  const hh = state === "idle" ? 8 : h + 6;
  invoke("set_hit_rect", { x: (WIN_W - hw) / 2, y: 0, w: hw, h: hh });
}

function scheduleHide() {
  clearTimeout(hideTimer);
  hideTimer = window.setTimeout(() => !hovered && setState("idle"), HIDE_AFTER_MS);
}

listen<boolean>("hover", ({ payload }) => {
  hovered = payload;
  if (hovered) {
    clearTimeout(hideTimer);
    if (state === "idle") {
      clearTimeout(dwellTimer);
      dwellTimer = window.setTimeout(
        () => hovered && setState("peek"),
        fullscreen ? DWELL_FULLSCREEN_MS : DWELL_MS,
      );
    }
  } else {
    clearTimeout(dwellTimer);
    if (state !== "idle") scheduleHide();
  }
});
listen<boolean>("fullscreen", ({ payload }) => (fullscreen = payload));

// Click anywhere on the peek that isn't a button -> open the panel.
island.addEventListener("click", (e) => {
  if ((e.target as Element).closest("button")) return;
  if (state === "peek") setState("open");
});
document.addEventListener("contextmenu", (e) => e.preventDefault());

// ---------- tabs ----------

let tab = 0;
const lines = $("nav.lines");
TABS.forEach(([, label], i) => {
  const b = document.createElement("button");
  b.setAttribute("aria-label", label);
  b.addEventListener("click", () => setTab(i));
  lines.append(b);
});
const tabEls = TABS.map(([id]) => $(`.tab[data-tab="${id}"]`));
const tabLabel = $(".tab-label");
const src = $(".src");

function setTab(i: number, instant = false) {
  i = (i + TABS.length) % TABS.length;
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
  src.classList.toggle("hide", i !== 0 || !media.has);
}

// ---------- scroll: volume (vertical) and tabs (horizontal swipe) ----------

let volPending = 0;
let volFlush = 0;
let volHudTimer = 0;
let swipeAcc = 0;
let swipeLock = 0;

document.addEventListener(
  "wheel",
  (e) => {
    const unit = e.deltaMode === 1 ? 33 : 1; // lines -> px
    const dx = e.deltaX * unit;
    const dy = e.deltaY * unit;

    if (Math.abs(dx) > Math.abs(dy)) {
      if (state !== "open" || performance.now() < swipeLock) return;
      swipeAcc += dx;
      if (Math.abs(swipeAcc) > 60) {
        setTab(tab + Math.sign(swipeAcc));
        swipeAcc = 0;
        swipeLock = performance.now() + 380;
      }
      return;
    }

    if (state === "idle") {
      clearTimeout(dwellTimer);
      setState("peek");
    }
    volPending += (-dy / 100) * VOLUME_PER_NOTCH;
    if (!volFlush) {
      volFlush = requestAnimationFrame(async () => {
        const delta = volPending;
        volPending = 0;
        volFlush = 0;
        const v = await invoke<{ level: number; muted: boolean }>("volume_step", { delta });
        showVolume(v.level, v.muted);
      });
    }
  },
  { passive: true },
);

function showVolume(level: number, muted: boolean) {
  island.style.setProperty("--v", String(muted ? 0 : level));
  island.classList.add("vhud");
  setText("peekSub", muted ? "Muted" : `Volume ${Math.round(level * 100)}%`);
  clearTimeout(volHudTimer);
  volHudTimer = window.setTimeout(() => {
    island.classList.remove("vhud");
    render();
  }, 1100);
}

// ---------- media ----------

let media: Media = {
  has: false, title: "", artist: "", album: "", app: "", playing: false,
  canPrev: false, canNext: false, position: 0, duration: 0, sampledAt: 0,
};

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
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
};

function render() {
  const m = media;
  setText("title", m.has ? m.title || "Unknown title" : "Nothing playing");
  setText("artist", m.has ? m.artist || m.app : "Play something in Spotify, YouTube, anywhere");
  if (!island.classList.contains("vhud")) setText("peekSub", m.has ? m.artist || m.app : "Hover here any time");
  setText("app", m.app);
  src.classList.toggle("live", m.playing);
  src.classList.toggle("hide", tab !== 0 || !m.has);

  $$<SVGUseElement>(".pp use").forEach((u) => u.setAttribute("href", m.playing ? "#i-pause" : "#i-play"));
  $$<HTMLButtonElement>('[data-act="prev"]').forEach((b) => (b.disabled = !m.canPrev));
  $$<HTMLButtonElement>('[data-act="next"]').forEach((b) => (b.disabled = !m.canNext));
  $$<HTMLButtonElement>('[data-act="toggle"]').forEach((b) => (b.disabled = !m.has));
  $(".progress").classList.toggle("off", !m.has || m.duration <= 0);
  tick();
  requestAnimationFrame(updateMarquees);
}

listen<Media>("media", ({ payload }) => {
  media = payload;
  render();
});

// album art (+ accent colour pulled from it)
const artBoxes = $$(".art");
const artImgs = $$<HTMLImageElement>(".art img");
listen<string | null>("media-art", ({ payload }) => {
  artBoxes.forEach((a) => a.classList.toggle("has", !!payload));
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
    const visible = t.closest(".layer")?.matches("#island[data-state=peek] .peek, #island[data-state=open] .panel");
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

// ---------- boot ----------

setTab(0, true);
render();
reportHitRect();
invoke<boolean>("ui_ready").then((fs) => (fullscreen = fs));
invoke<{ level: number; muted: boolean }>("volume_get").then((v) =>
  island.style.setProperty("--v", String(v.muted ? 0 : v.level)),
);
