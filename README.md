<div align="center">

# Perch

**A tiny media island that lives under your webcam on Windows 11.**

Push your cursor to the top-center of the screen and Perch drops down: what's playing, play/pause/skip,
and volume on the scroll wheel. Click it and it opens into a small tabbed panel. Leave it alone and it tucks
itself back up under the camera.

`~3 MB app` · `< 80 MB RAM` · `~0% CPU at rest` · `no player of its own: it controls whatever Windows is playing`

</div>

---

## How it feels

| State | What you see | How you get there |
|---|---|---|
| **Rest** | A thin black sliver under the camera, so it reads as part of the bezel | Default |
| **Peek** | Album art, title, artist, prev / play / next | Hover the top-center of the screen |
| **Panel** | Player, Apps, Shelf, Clipboard and more, switched with the little lines at the top | Click the peek anywhere except a button |
| **Drop** | "Drop to keep on Shelf", then a check mark when it lands | Drag any file to the top-center |

- **Scroll anywhere on it** to change the volume. Wheel up or two fingers up = louder, whatever your Windows
  scrolling direction is.
- **Ctrl + scroll** (or hold right-click + scroll) changes **screen brightness**.
- **Swipe sideways** on a touchpad to switch tabs.
- It **retracts 2.5 s** after the cursor leaves. Come back during that time and it stays open.
- **Fullscreen games and videos:** you have to hold the cursor there a little longer, so it never pops up by accident.
- Everything moves on **real spring physics** (damped-spring curves compiled to CSS `linear()`), not canned ease-in-outs.
- The player is **black where it meets the camera and melts into frosted glass** made from the album cover; the
  progress bar and the glow behind the cover take their colour from it too.

## Tabs

- **Now playing**: cover, title, artist, seek bar, controls.
- **Apps**: up to six quick-launch apps or websites. Press **Add**, type to search everything installed (or type a
  web address), Enter to add. **Edit** to remove.
- **Shelf**: files you dropped on the island. Click to open, **drag a tile out** to drop the file anywhere,
  right-click for Show in folder / Copy / Remove / Recycle. The header switches between keeping **links** to the
  originals (default) and parking **copies**. Empties on restart or shutdown, survives sleep.
- **Clipboard**: your last 5 copies (text, images with previews, files). Click one to copy it again.
  **Pause** stops recording; anything a password manager marks as private is never recorded. Memory only.
- **Sound**: every app that's making noise, each with its own level ring. **Scroll over an app** to change only
  that app's volume. With more than one output, the header switches between speakers and headphones.
- **Quick**: Wi-Fi, Bluetooth, mic mute, dark mode, a timer, and a shortcut to Settings. Battery level sits in the
  header.

## Live activities

While Perch is tucked away it can still say something:

- **Timer**: a small countdown pill stays under the camera, with a progress line. When it's done the island
  pulses and chimes; click it to dismiss.
- **Charger**: plugging in (or out) blooms the island for a moment with the battery level.

## The Perch window

Open it from the tray icon, the Quick tab, or by simply launching Perch again while it's running
(only one Perch ever runs). Inside:

- **On / off**: a master switch. Off means the island is gone and nothing is tracked; Perch waits in the tray.
- **Start with Windows**.
- **Look**: three themes. *Obsidian* (pure black, part of the bezel), *Graphite* (soft charcoal below the
  camera) and *Aurora* (glows with the album colour), plus the glass player on or off.
- **Island**: how long it waits before tucking away, and what happens during fullscreen games and videos
  (longer hold, hide completely, or as usual).
- A section for **every tab**: volume per notch and touchpad direction, reorder or remove apps, shelf link/copy
  mode, clipboard size and pause, output device, mic and dark mode.
- A live **memory meter**, so you can check the "light" claim yourself.

## What it controls

Anything that shows up in Windows' media controls: **Spotify, YouTube in Chrome/Edge/Firefox, Apple Music, VLC,
Media Player, TIDAL, Deezer** and more, including their cover art and thumbnails.

With several apps open, Perch follows whichever **started playing most recently**. When more than one is around,
the app name in the panel header turns into a picker so you can choose yourself; your choice sticks until you
start something else.

## Roadmap

- [x] Island window pinned under the webcam, click-through everywhere except the island itself
- [x] Peek → panel → auto-retract, with dwell so it never opens by accident
- [x] Live now-playing from any app, with controls, seeking and album art
- [x] Scroll-to-volume
- [x] Tab lines + spring transitions
- [x] Low-memory mode while at rest
- [x] **Apps**: quick launch with search
- [x] **Shelf**: drag in, drag out, open, recycle; link or copy mode
- [x] **Clipboard**: text, images and files with previews, pause, password-manager exclusion
- [x] Brightness on Ctrl + scroll, source picker for multiple players, glass player
- [x] **Sound**: per-app volume and output device switching
- [x] **Quick**: Wi-Fi, Bluetooth, mic, dark mode, battery, timer
- [x] **Live activities**: timer countdown, charger
- [x] **The Perch window**: on/off, start with Windows, themes, a section for every tab
- [x] Single instance, recovers by itself if the web view crashes
- [ ] **Lyrics**: time-synced lyrics via [LRCLIB](https://lrclib.net) when available
- [ ] Mic/camera-in-use dot right next to the actual camera
- [ ] Installer

## Why it's light

Perch is a [Tauri 2](https://tauri.app) app: a small Rust core does all the system work and a tiny, framework-free
TypeScript UI draws it.

- **No global mouse hook.** The window is click-through; a background thread checks the cursor position ~30×/s
  (microseconds of work) and makes only the island itself clickable.
- **Event-driven media.** Windows pushes now-playing changes; nothing is polled while music is paused.
- **Trimmed WebView2.** Software rendering for a surface this small, a single renderer, background services off,
  JIT-less JavaScript with a capped heap, and WebView2's low-memory target level whenever the island is at rest.
- **Nothing drawn at rest.** Tucked away, every panel is removed from the page; after a while at rest the page
  quietly reloads to hand back memory (all state lives in the Rust core, so nothing is lost).
- **No shell work on startup.** App icons are cached on disk; Windows' icon/thumbnail handlers are only loaded
  when you actually search or drop a file, and unloaded afterwards.
- **No frameworks.** The entire UI is about 35 KB of JavaScript.

Measured on a 1920×1080 laptop, release build, whole process tree including WebView2: **~76 MB at rest**, up to
~100 MB while you're actively using the panel. The Perch window is a separate page that only exists while it's
open (about +35 MB), and is gone completely once you close it.

## Build it

Requirements: Windows 11, [Node.js](https://nodejs.org) 20+, [Rust](https://rustup.rs), and the Visual Studio C++ build tools.

```sh
npm install
npm run tauri dev                      # run with hot reload
npx tauri build --no-bundle            # release exe in src-tauri/target/release/perch.exe
```

Quit from the tray icon or the Perch window.

## Project layout

```
index.html           the island            src/main.ts, src/styles.css
settings.html        the Perch window      src/settings.ts, src/settings.css
src-tauri/src/
  tracker.rs         window placement, cursor tracking, click-through, fullscreen, charger
  media.rs           System Media Transport Controls (now playing, sources, controls, art)
  audio.rs           Core Audio: master, per-app volume, output devices, mic
  brightness.rs      built-in display brightness (WMI)
  system.rs          battery, dark mode, radios, start with Windows, memory meter
  shell.rs           icons, app list, launching, Recycle Bin (one STA worker)
  shelf.rs           the file shelf (memory only)
  clipboard.rs       clipboard history (memory only)
  settings.rs        preferences, the only thing saved to disk
  lib.rs             commands, tray, windows, wiring
```

## License

[MIT](LICENSE)
