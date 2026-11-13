//! Cursor tracking. The window is click-through by default; it only becomes
//! interactive while the cursor is inside the rectangle the UI reports.
//! Polling GetCursorPos ~30x/s costs effectively nothing and avoids a global
//! mouse hook (hooks add latency to every mouse move system-wide).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use tauri::{Emitter, PhysicalPosition, PhysicalSize, WebviewWindow};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Shell::{
    QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

/// Logical size of the (transparent) host window. The island draws inside it.
const WIN_W: f64 = 460.0;
const WIN_H: f64 = 190.0;

#[derive(Clone, Copy, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Default)]
pub struct Shared {
    pub hit: Mutex<Rect>,
    pub fullscreen: AtomicBool,
    pub hovered: AtomicBool,
}

#[derive(PartialEq, Clone, Copy)]
struct Geom {
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    scale: f64,
}

/// Pin the window to the top-center of the primary monitor. Returns None if
/// the monitor can't be queried (e.g. mid display change).
fn geometry(win: &WebviewWindow) -> Option<Geom> {
    let m = win.primary_monitor().ok().flatten()?;
    let scale = m.scale_factor();
    let w = (WIN_W * scale).round() as u32;
    let h = (WIN_H * scale).round() as u32;
    let x = m.position().x + (m.size().width as i32 - w as i32) / 2;
    Some(Geom { x, y: m.position().y, w, h, scale })
}

fn is_fullscreen() -> bool {
    match unsafe { SHQueryUserNotificationState() } {
        Ok(s) => s == QUNS_BUSY || s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE,
        Err(_) => false,
    }
}

pub fn spawn(win: WebviewWindow, shared: std::sync::Arc<Shared>) {
    thread::spawn(move || {
        let _ = win.set_ignore_cursor_events(true);
        let mut geom: Option<Geom> = None;
        let mut inside = false;
        let mut tick: u32 = 0;

        loop {
            // Re-check monitor layout + fullscreen state every ~2s.
            if tick % 60 == 0 {
                let g = geometry(&win);
                if g.is_some() && g != geom {
                    let n = g.unwrap();
                    let _ = win.set_size(PhysicalSize::new(n.w, n.h));
                    let _ = win.set_position(PhysicalPosition::new(n.x, n.y));
                    if geom.is_none() {
                        let _ = win.show();
                    }
                    geom = g;
                }
                let fs = is_fullscreen();
                if shared.fullscreen.swap(fs, Ordering::Relaxed) != fs {
                    let _ = win.emit("fullscreen", fs);
                }
            }
            tick = tick.wrapping_add(1);

            if let Some(g) = geom {
                let mut p = POINT::default();
                if unsafe { GetCursorPos(&mut p) }.is_ok() {
                    let lx = (p.x - g.x) as f64 / g.scale;
                    let ly = (p.y - g.y) as f64 / g.scale;
                    let r = *shared.hit.lock().unwrap();
                    let now = lx >= r.x && lx <= r.x + r.w && ly >= r.y && ly <= r.y + r.h;
                    if now != inside {
                        inside = now;
                        shared.hovered.store(now, Ordering::Relaxed);
                        let _ = win.set_ignore_cursor_events(!now);
                        let _ = win.emit("hover", now);
                    }
                }
            }

            // Poll faster while hovered so leaving feels instant.
            thread::sleep(Duration::from_millis(if inside { 16 } else { 33 }));
        }
    });
}
