//! Clipboard history. Memory-only: it empties on restart/shutdown but survives
//! sleep. Content that password managers mark as private is never recorded.

use crate::{settings::Store, shell};
use clipboard_win::{Clipboard, Monitor, Setter, formats, get_clipboard, is_format_avail, raw, register_format, set_clipboard};
use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub id: u32,
    /// "text" | "image" | "files"
    pub kind: String,
    pub text: Option<String>,
    pub thumb: Option<String>,
    pub files: Vec<String>,
    pub detail: String,
}

enum Payload {
    Text(String),
    Image(Vec<u8>), // PNG, full size
    Files(Vec<String>),
}

struct Entry {
    view: View,
    payload: Payload,
    hash: u64,
}

pub struct History {
    entries: Mutex<Vec<Entry>>,
    next: AtomicU32,
}

impl History {
    pub fn views(&self) -> Vec<View> {
        self.entries.lock().unwrap().iter().map(|e| e.view.clone()).collect()
    }

    pub fn remove(&self, id: u32) {
        self.entries.lock().unwrap().retain(|e| e.view.id != id);
    }

    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }

    pub fn trim(&self, max: usize) {
        self.entries.lock().unwrap().truncate(max.max(1));
    }

    /// Put an entry back on the system clipboard. The monitor then sees it,
    /// recognises the hash and simply moves it to the front.
    pub fn copy(&self, id: u32) -> bool {
        let entries = self.entries.lock().unwrap();
        let Some(e) = entries.iter().find(|e| e.view.id == id) else { return false };
        match &e.payload {
            Payload::Text(t) => set_clipboard(formats::Unicode, t).is_ok(),
            Payload::Files(f) => set_files(f),
            Payload::Image(png) => {
                let Ok(img) = image::load_from_memory_with_format(png, image::ImageFormat::Png) else { return false };
                let mut bmp = std::io::Cursor::new(Vec::new());
                if img.to_rgba8().write_to(&mut bmp, image::ImageFormat::Bmp).is_err() {
                    return false;
                }
                set_clipboard(formats::Bitmap, bmp.into_inner()).is_ok()
            }
        }
    }
}

/// Put a list of files on the clipboard (as if copied in Explorer).
pub fn set_files<S: AsRef<str>>(paths: &[S]) -> bool {
    let Ok(_c) = Clipboard::new_attempts(10) else { return false };
    let _ = raw::empty();
    formats::FileList.write_clipboard(paths).is_ok()
}

fn hash_of<T: Hash>(t: &T) -> u64 {
    let mut h = DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

/// Formats apps set to say "don't record this" (KeePass, 1Password, Bitwarden, Windows itself…).
fn is_private() -> bool {
    for name in ["ExcludeClipboardContentFromMonitorProcessing", "Clipboard Viewer Ignore"] {
        if let Some(f) = register_format(name) {
            if is_format_avail(f.get()) {
                return true;
            }
        }
    }
    if let Some(f) = register_format("CanIncludeInClipboardHistory") {
        if is_format_avail(f.get()) {
            if let Ok(v) = get_clipboard(formats::RawData(f.get())) {
                if v.len() >= 4 && u32::from_le_bytes([v[0], v[1], v[2], v[3]]) == 0 {
                    return true;
                }
            }
        }
    }
    false
}

fn read() -> Option<(Payload, View, u64)> {
    if is_private() {
        return None;
    }
    let blank = |kind: &str| View {
        id: 0,
        kind: kind.into(),
        text: None,
        thumb: None,
        files: vec![],
        detail: String::new(),
    };

    if let Ok(files) = get_clipboard::<Vec<String>, _>(formats::FileList) {
        if !files.is_empty() {
            let mut v = blank("files");
            v.files = files
                .iter()
                .map(|f| f.rsplit(['\\', '/']).next().unwrap_or(f).to_string())
                .collect();
            v.detail = if files.len() == 1 { "File".into() } else { format!("{} files", files.len()) };
            v.thumb = shell::image_png(&files[0], 96, false).as_deref().map(shell::data_url);
            let h = hash_of(&files);
            return Some((Payload::Files(files), v, h));
        }
    }

    if let Ok(text) = get_clipboard::<String, _>(formats::Unicode) {
        if !text.trim().is_empty() {
            let mut v = blank("text");
            let preview: String = text.trim().chars().take(240).collect();
            v.text = Some(preview);
            let n = text.chars().count();
            v.detail = if text.starts_with("http://") || text.starts_with("https://") {
                "Link".into()
            } else {
                format!("{n} chars")
            };
            let h = hash_of(&text);
            return Some((Payload::Text(text), v, h));
        }
    }

    if let Ok(bmp) = get_clipboard::<Vec<u8>, _>(formats::Bitmap) {
        let img = image::load_from_memory_with_format(&bmp, image::ImageFormat::Bmp).ok()?;
        let rgba = img.to_rgba8();
        let h = hash_of(&rgba.as_raw());
        let mut v = blank("image");
        v.detail = format!("{} × {}", rgba.width(), rgba.height());
        let thumb = image::DynamicImage::ImageRgba8(rgba.clone()).thumbnail(220, 150);
        let mut t = std::io::Cursor::new(Vec::new());
        thumb.write_to(&mut t, image::ImageFormat::Png).ok()?;
        v.thumb = Some(shell::data_url(&t.into_inner()));
        let mut full = std::io::Cursor::new(Vec::new());
        rgba.write_to(&mut full, image::ImageFormat::Png).ok()?;
        return Some((Payload::Image(full.into_inner()), v, h));
    }
    None
}

pub fn spawn(app: AppHandle, store: Arc<Store>) -> Arc<History> {
    let hist = Arc::new(History { entries: Mutex::new(Vec::new()), next: AtomicU32::new(0) });
    let h = hist.clone();
    thread::spawn(move || {
        let Ok(mut mon) = Monitor::new() else { return };
        while mon.recv().is_ok() {
            let s = store.get();
            if s.clipboard_paused {
                continue;
            }
            // Let the copying app finish writing all of its formats.
            thread::sleep(Duration::from_millis(60));
            let Some((payload, mut view, hash)) = read() else { continue };
            {
                let mut entries = h.entries.lock().unwrap();
                if let Some(pos) = entries.iter().position(|e| e.hash == hash) {
                    let e = entries.remove(pos);
                    entries.insert(0, e);
                } else {
                    view.id = h.next.fetch_add(1, Ordering::Relaxed) + 1;
                    entries.insert(0, Entry { view, payload, hash });
                    entries.truncate(s.clipboard_size.max(1));
                }
            }
            let _ = app.emit("clipboard", h.views());
        }
    });
    hist
}
