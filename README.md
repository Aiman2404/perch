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
| **Panel** | The full player and the other tabs, switched with the little lines at the top | Click the peek anywhere except a button |

- **Scroll anywhere on it** to change the volume (mouse wheel or two-finger swipe). A thin bar shows the level.
- **Swipe sideways** on a touchpad to switch tabs.
- It **retracts 2.5 s** after the cursor leaves. Come back during that time and it stays open.
- **Fullscreen games and videos:** you have to hold the cursor there a little longer, so it never pops up by accident.
- Everything moves on **real spring physics** (damped-spring curves compiled to CSS `linear()`), not canned ease-in-outs.
- The progress bar and the glow behind the cover take their **colour from the album art**.

## What it controls

Anything that shows up in Windows' media controls: **Spotify, YouTube in Chrome/Edge/Firefox, Apple Music, VLC,
Media Player, TIDAL, Deezer** and more, including their cover art and thumbnails. If two apps are open, Perch
follows the one that's actually playing.

## Roadmap

- [x] Island window pinned under the webcam, click-through everywhere except the island itself
- [x] Peek → panel → auto-retract, with dwell so it never opens by accident
- [x] Live now-playing from any app, with controls, seeking and album art
- [x] Scroll-to-volume
- [x] Tab lines + spring transitions
- [x] Low-memory mode while at rest
- [ ] **Shelf**: drag a file onto the notch, let go, and it's parked there until you drag it out
- [ ] **Clipboard**: recent copies, text and images, with a pause switch and password-manager exclusion
- [ ] **Sound**: per-app volume and output device switching
- [ ] **Quick**: Wi-Fi, Bluetooth, Do Not Disturb, dark mode, battery, focus timer
- [ ] **Lyrics**: time-synced lyrics via [LRCLIB](https://lrclib.net) when available
- [ ] **Live activities**: timer, charging, mic/camera-in-use dot right next to the actual camera
- [ ] **Settings** with a section for every tab, plus themes (Obsidian, Frost glass, Aurora glass)
- [ ] Start with Windows, installer

## Why it's light

Perch is a [Tauri 2](https://tauri.app) app: a small Rust core does all the system work and a tiny, framework-free
TypeScript UI draws it.

- **No global mouse hook.** The window is click-through; a background thread checks the cursor position ~30×/s
  (microseconds of work) and makes only the island itself clickable.
- **Event-driven media.** Windows pushes now-playing changes; nothing is polled while music is paused.
- **Trimmed WebView2.** Software rendering for a surface this small, a single renderer, background services off,
  a capped JS heap, and WebView2's low-memory target level whenever the island is at rest.
- **No frameworks.** The entire UI is about 8 KB of JavaScript.

Measured on a 1920×1080 laptop, release build, at rest: **~77 MB private memory** for the whole process tree,
including WebView2.

## Build it

Requirements: Windows 11, [Node.js](https://nodejs.org) 20+, [Rust](https://rustup.rs), and the Visual Studio C++ build tools.

```sh
npm install
npm run tauri dev                      # run with hot reload
npx tauri build --no-bundle            # release exe in src-tauri/target/release/perch.exe
```

Quit from the tray icon.

## Project layout

```
src/                 UI: index.html, styles.css, main.ts
src-tauri/src/
  tracker.rs         window placement, cursor tracking, click-through, fullscreen detection
  media.rs           System Media Transport Controls (now playing, controls, art)
  audio.rs           master volume via Core Audio
  lib.rs             commands, tray, wiring
```

## License

[MIT](LICENSE)
