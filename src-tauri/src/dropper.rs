//! Things dragged onto the island.
//!
//! Tauri's own drop handling only accepts real files on disk. Pictures dragged out of a
//! browser, WhatsApp, Telegram, the Photos app or a screenshot preview are not files yet:
//! they arrive as "virtual files" (FileGroupDescriptor + FileContents) or as a bitmap. This
//! drop target accepts those too and saves them to the shelf's temporary folder first.

use crate::{shelf, Shelf, Store};
use serde::Serialize;
use std::cell::Cell;
use std::ffi::{c_void, OsString};
use std::os::windows::ffi::OsStringExt;
use std::path::Path;
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use windows::core::{implement, Ref, BOOL, PCWSTR};
use windows::Win32::Foundation::{HGLOBAL, HWND, LPARAM, POINTL};
use windows::Win32::System::Com::{
    IDataObject, IStream, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL, TYMED_ISTREAM,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_DIB, CF_HDROP,
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{
    DragQueryFileW, IShellItemArray, SHCreateShellItemArrayFromDataObject, FILEDESCRIPTORW,
    FILEGROUPDESCRIPTORW, HDROP, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::EnumChildWindows;

#[derive(Serialize, Clone)]
struct Enter {
    count: usize,
    name: String,
}

/// Replace the webview's drop targets with ours. Call on the main thread once the window exists.
pub fn install(win: &WebviewWindow) {
    let Ok(top) = win.hwnd() else { return };
    let top = HWND(top.0 as _);
    let mut hwnds: Vec<HWND> = vec![top];
    unsafe extern "system" fn collect(h: HWND, l: LPARAM) -> BOOL {
        unsafe { (*(l.0 as *mut Vec<HWND>)).push(h) };
        true.into()
    }
    unsafe {
        let _ = EnumChildWindows(Some(top), Some(collect), LPARAM(&mut hwnds as *mut _ as isize));
    }
    for h in hwnds {
        let target: IDropTarget = Target { app: win.app_handle().clone(), valid: Cell::new(false) }.into();
        unsafe {
            let _ = RevokeDragDrop(h);
            let _ = RegisterDragDrop(h, &target);
        }
        // the window keeps its own reference to the target for as long as it is registered
    }
}

#[implement(IDropTarget)]
struct Target {
    app: AppHandle,
    valid: Cell<bool>,
}

fn fmt(cf: u16, tymed: i32, index: i32) -> FORMATETC {
    FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: index, tymed: tymed as u32 }
}

fn cf(name: &str) -> u16 {
    let w: Vec<u16> = name.encode_utf16().chain([0]).collect();
    unsafe { RegisterClipboardFormatW(PCWSTR(w.as_ptr())) as u16 }
}

fn has(obj: &IDataObject, f: &FORMATETC) -> bool {
    unsafe { obj.QueryGetData(f).is_ok() }
}

/// Copy an HGLOBAL's bytes.
unsafe fn global_bytes(h: HGLOBAL) -> Vec<u8> {
    unsafe {
        let size = GlobalSize(h);
        let p = GlobalLock(h) as *const u8;
        if p.is_null() {
            return Vec::new();
        }
        let v = std::slice::from_raw_parts(p, size).to_vec();
        let _ = GlobalUnlock(h);
        v
    }
}

unsafe fn stream_bytes(s: &IStream) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let mut read = 0u32;
        let hr = unsafe { s.Read(buf.as_mut_ptr() as *mut c_void, buf.len() as u32, Some(&mut read)) };
        if hr.is_err() || read == 0 {
            break;
        }
        out.extend_from_slice(&buf[..read as usize]);
    }
    out
}

/// Bytes of one format, whether it comes as memory or as a stream.
fn data(obj: &IDataObject, cf: u16, index: i32) -> Option<Vec<u8>> {
    unsafe {
        let mut m: STGMEDIUM = obj.GetData(&fmt(cf, TYMED_HGLOBAL.0 | TYMED_ISTREAM.0, index)).ok()?;
        let bytes = if m.tymed == TYMED_HGLOBAL.0 as u32 {
            global_bytes(m.u.hGlobal)
        } else if m.tymed == TYMED_ISTREAM.0 as u32 {
            match &*m.u.pstm {
                Some(s) => stream_bytes(s),
                None => Vec::new(),
            }
        } else {
            Vec::new()
        };
        ReleaseStgMedium(&mut m);
        (!bytes.is_empty()).then_some(bytes)
    }
}

fn real_paths(obj: &IDataObject) -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        if let Ok(mut m) = obj.GetData(&fmt(CF_HDROP.0, TYMED_HGLOBAL.0, -1)) {
            let h = HDROP(m.u.hGlobal.0);
            for i in 0..DragQueryFileW(h, u32::MAX, None) {
                let n = DragQueryFileW(h, i, None) as usize;
                let mut buf = vec![0u16; n + 1];
                DragQueryFileW(h, i, Some(&mut buf));
                out.push(OsString::from_wide(&buf[..n]).to_string_lossy().into_owned());
            }
            ReleaseStgMedium(&mut m);
        }
        if out.is_empty() {
            // Explorer can't build CF_HDROP for very long paths; the shell item array has no limit.
            if let Ok(items) = SHCreateShellItemArrayFromDataObject::<_, IShellItemArray>(obj) {
                for i in 0..items.GetCount().unwrap_or(0) {
                    if let Ok(name) = items.GetItemAt(i).and_then(|it| it.GetDisplayName(SIGDN_FILESYSPATH)) {
                        out.push(name.to_string().unwrap_or_default());
                        windows::Win32::System::Com::CoTaskMemFree(Some(name.0 as _));
                    }
                }
            }
        }
    }
    out.retain(|p| !p.is_empty());
    out
}

/// Names of virtual files, if the drag carries any.
fn virtual_names(obj: &IDataObject) -> Vec<String> {
    let Some(bytes) = data(obj, cf("FileGroupDescriptorW"), -1) else { return Vec::new() };
    if bytes.len() < 4 {
        return Vec::new();
    }
    let count = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    let size = std::mem::size_of::<FILEDESCRIPTORW>();
    let first = std::mem::offset_of!(FILEGROUPDESCRIPTORW, fgd);
    (0..count)
        .filter_map(|i| {
            let at = first + i * size;
            let d = bytes.get(at..at + size)?;
            let fd = unsafe { std::ptr::read_unaligned(d.as_ptr() as *const FILEDESCRIPTORW) };
            let name = { fd.cFileName }; // copy out of the packed struct
            let n = name.iter().position(|&c| c == 0).unwrap_or(name.len());
            Some(String::from_utf16_lossy(&name[..n]))
        })
        .collect()
}

fn clean_name(name: &str, fallback: &str) -> String {
    let n: String = name
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or("")
        .chars()
        .map(|c| if "<>:\"|?*".contains(c) || c.is_control() { '_' } else { c })
        .collect();
    let n = n.trim().trim_end_matches('.').to_string();
    if n.is_empty() { fallback.to_string() } else { n }
}

fn save(dir: &Path, name: &str, bytes: &[u8]) -> Option<String> {
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(name);
    std::fs::write(&path, bytes).ok()?;
    Some(path.to_string_lossy().into_owned())
}

/// A bare bitmap (CF_DIB) becomes a .bmp file.
fn dib_to_bmp(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }
    let header = u32::from_le_bytes(dib[0..4].try_into().ok()?) as usize;
    let bits = u16::from_le_bytes(dib[14..16].try_into().ok()?) as usize;
    let compression = u32::from_le_bytes(dib[16..20].try_into().ok()?);
    let used = u32::from_le_bytes(dib[32..36].try_into().ok()?) as usize;
    let palette = if bits <= 8 { (if used > 0 { used } else { 1 << bits }) * 4 } else { 0 };
    let masks = if compression == 3 && header == 40 { 12 } else { 0 };
    let offset = 14 + header + masks + palette;
    let mut out = Vec::with_capacity(14 + dib.len());
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((14 + dib.len()) as u32).to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(offset as u32).to_le_bytes());
    out.extend_from_slice(dib);
    Some(out)
}

/// Everything in the drag, as paths on disk. Virtual files and bitmaps are saved first.
fn collect(obj: &IDataObject) -> Vec<String> {
    let real = real_paths(obj);
    if !real.is_empty() {
        return real;
    }
    let dir = shelf::incoming_dir();
    let names = virtual_names(obj);
    if !names.is_empty() {
        let contents = cf("FileContents");
        let mut used = std::collections::HashSet::new();
        return names
            .iter()
            .enumerate()
            .filter_map(|(i, n)| {
                let bytes = data(obj, contents, i as i32)?;
                let mut name = clean_name(n, "Picture.png");
                while !used.insert(name.to_lowercase()) {
                    name = format!("{} {}", i + 1, name);
                }
                save(&dir, &name, &bytes)
            })
            .collect();
    }
    if let Some(png) = data(obj, cf("PNG"), -1) {
        return save(&dir, "Picture.png", &png).into_iter().collect();
    }
    if let Some(bmp) = data(obj, CF_DIB.0, -1).and_then(|d| dib_to_bmp(&d)) {
        return save(&dir, "Picture.bmp", &bmp).into_iter().collect();
    }
    Vec::new()
}

/// What to show while hovering. None when there's nothing we can keep.
fn preview(obj: &IDataObject) -> Option<Enter> {
    let real = real_paths(obj);
    if !real.is_empty() {
        let name = Path::new(&real[0]).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return Some(Enter { count: real.len(), name });
    }
    let names = virtual_names(obj);
    if !names.is_empty() {
        return Some(Enter { count: names.len(), name: clean_name(&names[0], "Picture") });
    }
    if has(obj, &fmt(cf("PNG"), TYMED_HGLOBAL.0 | TYMED_ISTREAM.0, -1)) || has(obj, &fmt(CF_DIB.0, TYMED_HGLOBAL.0, -1)) {
        return Some(Enter { count: 1, name: "Picture".into() });
    }
    None
}

#[allow(non_snake_case)]
impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(
        &self,
        obj: Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let info = obj.as_ref().and_then(preview);
        self.valid.set(info.is_some());
        unsafe { *effect = if info.is_some() { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
        if let Some(info) = info {
            let _ = self.app.emit_to("main", "drop-enter", info);
        }
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        unsafe { *effect = if self.valid.get() { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        if self.valid.replace(false) {
            let _ = self.app.emit_to("main", "drop-leave", ());
        }
        Ok(())
    }

    fn Drop(
        &self,
        obj: Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        if !self.valid.replace(false) {
            unsafe { *effect = DROPEFFECT_NONE };
            return Ok(());
        }
        // The data only exists during this call, so read it now; icons are made off this thread.
        let paths = obj.as_ref().map(collect).unwrap_or_default();
        unsafe { *effect = DROPEFFECT_COPY };
        let app = self.app.clone();
        std::thread::spawn(move || {
            let shelf = app.state::<Shelf>();
            let copy = app.state::<Store>().get().shelf_mode == "copy";
            let got = paths.len();
            let n = if paths.is_empty() { 0 } else { shelf.add(paths, copy) };
            crate::emit_shelf(&app, &shelf);
            let _ = app.emit_to("main", "drop-done", serde_json::json!({ "added": n, "got": got }));
        });
        Ok(())
    }
}
