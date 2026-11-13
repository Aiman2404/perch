import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

interface Settings {
  enabled: boolean; theme: string; glassPlayer: boolean; retractMs: number; fullscreen: string; volumeStep: number;
  shelfMode: string; clipboardSize: number; clipboardPaused: boolean; touchpadInvert: boolean;
  quickApps: { name: string; target: string; kind: string }[] | null;
}
interface AppView { name: string; target: string; kind: string; icon: string | null }
interface Status { version: string; memoryMb: number; autostart: boolean }
interface Device { id: string; name: string; default: boolean }

const $ = <T extends Element = HTMLElement>(s: string, el: ParentNode = document) => el.querySelector(s) as T;
const $$ = <T extends Element = HTMLElement>(s: string, el: ParentNode = document) => [...el.querySelectorAll<T & Element>(s)] as T[];
const setText = (k: string, v: string) => $$(`[data-bind="${k}"]`).forEach((e) => (e.textContent = v));
const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]!);

const win = getCurrentWindow();
let s: Settings;

// ---------- window chrome ----------

$('[data-win="min"]').addEventListener("click", () => win.minimize());
$('[data-win="close"]').addEventListener("click", () => win.close());
document.addEventListener("keydown", (e) => e.key === "Escape" && win.close());
document.addEventListener("contextmenu", (e) => e.preventDefault());

// ---------- navigation ----------

function go(sec: string) {
  $$("[data-sec]").forEach((el) => el.classList.toggle("on", el.dataset.sec === sec));
  if (sec === "apps") loadApps();
  if (sec === "sound") loadDevices();
  if (sec === "quick") loadQuick();
}
$$("nav a").forEach((a) => a.addEventListener("click", () => go(a.dataset.sec!)));

// ---------- settings <-> controls ----------

async function patch(p: Partial<Settings>) {
  s = await invoke<Settings>("settings_patch", { patch: p });
  render();
}

function render() {
  document.body.dataset.theme = s.theme;
  document.body.classList.toggle("off", !s.enabled);
  $$<HTMLInputElement>("input[data-set]").forEach((i) => (i.checked = !!s[i.dataset.set as keyof Settings]));
  $$(".seg").forEach((seg) => {
    const v = String(s[seg.dataset.set as keyof Settings]);
    $$("button", seg).forEach((b) => b.classList.toggle("on", b.dataset.v === v));
  });
  $$(".theme").forEach((t) => t.classList.toggle("on", t.dataset.theme === s.theme));
  setText("heroTitle", s.enabled ? "Perch is on" : "Perch is off");
  setText(
    "heroSub",
    s.enabled
      ? "Sitting under your camera. Hover the top of the screen."
      : "Hidden and resting in the tray. Nothing is tracked while it's off.",
  );
}

$$<HTMLInputElement>("input[data-set]").forEach((i) =>
  i.addEventListener("change", () => patch({ [i.dataset.set!]: i.checked })),
);
$$(".seg").forEach((seg) =>
  seg.addEventListener("click", (e) => {
    const b = (e.target as Element).closest<HTMLElement>("button");
    if (!b) return;
    patch({ [seg.dataset.set!]: "num" in seg.dataset ? Number(b.dataset.v) : b.dataset.v });
  }),
);
$$(".theme").forEach((t) => t.addEventListener("click", () => patch({ theme: t.dataset.theme })));

// The island can change settings too (shelf mode, clipboard pause), so keep in step.
listen<Settings>("settings", ({ payload }) => {
  s = payload;
  render();
});

// ---------- general ----------

const auto = $<HTMLInputElement>("input[data-auto]");
auto.addEventListener("change", async () => {
  auto.checked = (await invoke<boolean>("autostart_set", { on: auto.checked })) ? auto.checked : !auto.checked;
});

$$("[data-act]").forEach((b) =>
  b.addEventListener("click", async () => {
    switch (b.dataset.act) {
      case "quit":
        return invoke("quit_app");
      case "shelf-clear":
        await invoke("shelf_clear");
        return done(b, "Emptied");
      case "clip-clear":
        await invoke("clip_clear");
        return done(b, "Cleared");
      case "github":
        return invoke("quick_launch", { target: "https://github.com/Aiman2404/perch", kind: "url" });
    }
  }),
);

function done(b: HTMLElement, text: string) {
  const old = b.textContent;
  b.textContent = text;
  b.classList.add("ok");
  setTimeout(() => {
    b.textContent = old;
    b.classList.remove("ok");
  }, 1400);
}

/** The memory meter is scaled to Perch's own 80 MB budget. */
async function poll() {
  const st = await invoke<Status>("status");
  setText("mem", st.memoryMb.toFixed(0));
  setText("version", `Perch ${st.version}`);
  $<HTMLElement>(".meter i").style.setProperty("--m", String(Math.min(1, st.memoryMb / 120)));
  $(".mem").classList.toggle("over", st.memoryMb > 80);
  auto.checked = st.autostart;
}

// ---------- updates ----------

interface Upd { current: string; available: string | null; checking: boolean; error: string | null }
const updBtn = $("[data-upd]");
let upd: Upd | null = null;
let installing = false;

function renderUpd() {
  if (!upd) return;
  const row = $(".row.upd");
  row.classList.toggle("ready", !!upd.available);
  if (installing) {
    setText("updText", `Installing ${upd.available}… Perch will restart by itself.`);
    updBtn.textContent = "Installing";
  } else if (upd.checking) {
    setText("updText", "Checking GitHub for a newer version…");
    updBtn.textContent = "Checking";
  } else if (upd.available) {
    setText("updText", `Perch ${upd.available} is ready. You have ${upd.current}.`);
    updBtn.textContent = "Install and restart";
  } else if (upd.error) {
    setText("updText", "Couldn't reach GitHub. Perch will try again later.");
    updBtn.textContent = "Try again";
  } else {
    setText("updText", `You have the latest version (${upd.current}). Perch checks every few hours.`);
    updBtn.textContent = "Check now";
  }
  updBtn.classList.toggle("primary", !!upd.available && !installing);
}

updBtn.addEventListener("click", async () => {
  if (!upd || upd.checking || installing) return;
  if (upd.available) {
    installing = true;
    renderUpd();
    try {
      await invoke("update_install");
    } catch {
      installing = false;
      setText("updText", "The update didn't install. Try again in a moment.");
    }
    return;
  }
  upd = await invoke<Upd>("update_check");
  renderUpd();
});
listen<Upd>("update", ({ payload }) => {
  upd = payload;
  renderUpd();
});
invoke<Upd>("update_status").then((u) => {
  upd = u;
  renderUpd();
});

// ---------- apps ----------

let apps: AppView[] = [];
async function loadApps() {
  apps = await invoke<AppView[]>("quick_apps");
  renderApps();
}
function renderApps() {
  const list = $(".apps-list");
  list.replaceChildren(
    ...apps.map((a, i) => {
      const row = document.createElement("div");
      row.className = "item";
      const ic = a.icon
        ? `<img src="${a.icon}" alt="" />`
        : `<span class="mono">${esc(a.name.charAt(0).toUpperCase())}</span>`;
      row.innerHTML = `<span class="ic">${ic}</span><span class="t"><b>${esc(a.name)}</b><small>${
        a.kind === "url" ? esc(a.target) : "App"
      }</small></span>
        <button class="mini-btn" data-m="up" ${i === 0 ? "disabled" : ""} aria-label="Move up"><svg viewBox="0 0 12 12"><path d="M3 7.5l3-3 3 3"/></svg></button>
        <button class="mini-btn" data-m="down" ${i === apps.length - 1 ? "disabled" : ""} aria-label="Move down"><svg viewBox="0 0 12 12"><path d="M3 4.5l3 3 3-3"/></svg></button>
        <button class="mini-btn" data-m="rm" aria-label="Remove"><svg viewBox="0 0 12 12"><path d="M3.5 3.5l5 5M8.5 3.5l-5 5"/></svg></button>`;
      row.addEventListener("click", async (e) => {
        const m = (e.target as Element).closest<HTMLElement>("[data-m]")?.dataset.m;
        if (!m) return;
        const next = [...apps];
        if (m === "rm") next.splice(i, 1);
        else {
          const j = m === "up" ? i - 1 : i + 1;
          [next[i], next[j]] = [next[j], next[i]];
        }
        apps = await invoke<AppView[]>("quick_apps_save", {
          apps: next.map(({ name, target, kind }) => ({ name, target, kind })),
        });
        renderApps();
      });
      return row;
    }),
  );
  if (!apps.length) list.innerHTML = `<p class="lead">No apps yet. Open the island's Apps tab and press Add.</p>`;
}

// ---------- sound ----------

async function loadDevices() {
  const st = await invoke<{ devices: Device[] }>("sound_state");
  const box = $(".devs");
  box.replaceChildren(
    ...st.devices.map((d) => {
      const b = document.createElement("button");
      b.className = "dev" + (d.default ? " on" : "");
      b.innerHTML = `<i></i><span>${esc(d.name)}</span>`;
      b.addEventListener("click", async () => {
        await invoke("device_set", { id: d.id });
        loadDevices();
      });
      return b;
    }),
  );
}

// ---------- quick ----------

async function loadQuick() {
  const q = await invoke<{ micMuted: boolean | null; dark: boolean }>("quick_state");
  const mic = $<HTMLInputElement>('[data-quick="mic"]');
  mic.checked = !!q.micMuted;
  mic.disabled = q.micMuted === null;
  $<HTMLInputElement>('[data-quick="dark"]').checked = q.dark;
}
$$<HTMLInputElement>("input[data-quick]").forEach((i) =>
  i.addEventListener("change", async () => {
    await invoke("quick_toggle", { which: i.dataset.quick, on: i.checked });
    loadQuick();
  }),
);

// ---------- boot ----------

invoke<Settings>("settings_get").then((v) => {
  s = v;
  render();
  requestAnimationFrame(() => document.body.classList.add("ready"));
});
poll();
setInterval(poll, 2000);
