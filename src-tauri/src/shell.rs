//! Windows Shell helpers. Shell thumbnail/icon handlers expect an STA thread,
//! so everything here runs on one dedicated worker thread.

use base64::Engine;
use std::ffi::c_void;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetObjectW, HGDIOBJ, ReleaseDC,
};
use windows::Win32::System::Com::{CoFreeUnusedLibrariesEx, CoTaskMemFree};
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
    FOLDERID_AppsFolder, IEnumShellItems, IShellItem, IShellItemImageFactory, KF_FLAG_DEFAULT,
    SHCreateItemFromParsingName, SHFILEOPSTRUCTW, SHFileOperationW, SHGetKnownFolderItem,
    SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING, SIIGBF_ICONONLY, SIIGBF_RESIZETOFIT, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

type Job = Box<dyn FnOnce() + Send>;
static WORKER: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();

fn worker() -> Sender<Job> {
    WORKER
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Job>();
            thread::spawn(move || {
                unsafe {
                    let _ = OleInitialize(None);
                }
                for job in rx {
                    job();
                    // Unload shell extensions / thumbnail handlers we no longer need.
                    unsafe { CoFreeUnusedLibrariesEx(0, None) };
                }
            });
            Mutex::new(tx)
        })
        .lock()
        .unwrap()
        .clone()
}

/// Run `f` on the shell thread and wait for its result.
pub fn call<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    let _ = worker().send(Box::new(move || {
        let _ = tx.send(f());
    }));
    rx.recv().expect("shell worker died")
}

pub fn data_url(png: &[u8]) -> String {
    format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png))
}

/// Icon (or, for files like images, a real thumbnail) as PNG bytes.
/// `parsing` is a file path or a shell path such as `shell:AppsFolder\<AUMID>`.
pub fn image_png(parsing: &str, size: i32, icon_only: bool) -> Option<Vec<u8>> {
    let parsing = parsing.to_string();
    call(move || unsafe {
        let f: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(parsing), None).ok()?;
        let mut flags = SIIGBF_RESIZETOFIT;
        if icon_only {
            flags |= SIIGBF_ICONONLY;
        }
        let hbm = f.GetImage(SIZE { cx: size, cy: size }, flags).ok()?;

        let mut bm = BITMAP::default();
        GetObjectW(
            HGDIOBJ(hbm.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut _ as *mut c_void),
        );
        let (w, h) = (bm.bmWidth, bm.bmHeight);
        if w <= 0 || h <= 0 {
            let _ = DeleteObject(HGDIOBJ(hbm.0));
            return None;
        }
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        let dc = GetDC(None);
        let lines = GetDIBits(dc, hbm, 0, h as u32, Some(px.as_mut_ptr() as *mut c_void), &mut bi, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        let _ = DeleteObject(HGDIOBJ(hbm.0));
        if lines == 0 {
            return None;
        }

        // BGRA (premultiplied) -> straight RGBA. Some bitmaps carry no alpha at all.
        let has_alpha = px.chunks_exact(4).any(|p| p[3] != 0);
        for p in px.chunks_exact_mut(4) {
            let (b, g, r, a) = (p[0], p[1], p[2], if has_alpha { p[3] } else { 255 });
            let un = |c: u8| if a == 0 || a == 255 { c } else { ((c as u32 * 255) / a as u32).min(255) as u8 };
            p[0] = un(r);
            p[1] = un(g);
            p[2] = un(b);
            p[3] = a;
        }
        let img = image::RgbaImage::from_raw(w as u32, h as u32, px)?;
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).ok()?;
        Some(out.into_inner())
    })
}

#[derive(serde::Serialize, Clone)]
pub struct AppEntry {
    pub name: String,
    pub id: String,
}

/// Everything in Start's "All apps" list (Win32 and Store apps alike).
pub fn list_apps() -> Vec<AppEntry> {
    call(|| unsafe {
        let mut out = Vec::new();
        let Ok(folder) = SHGetKnownFolderItem::<IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None) else {
            return out;
        };
        let Ok(en) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else {
            return out;
        };
        loop {
            let mut items = [None];
            let mut n = 0u32;
            if en.Next(&mut items, Some(&mut n)).is_err() || n == 0 {
                break;
            }
            let Some(item) = items[0].take() else { break };
            let name = display(&item, SIGDN_NORMALDISPLAY);
            let id = display(&item, SIGDN_PARENTRELATIVEPARSING);
            if name.is_empty() || id.is_empty() || name.to_lowercase().starts_with("uninstall") {
                continue;
            }
            // Parsing names come back relative to the Apps folder (the AUMID) or fully qualified.
            let id = id.rsplit("\\::").next().unwrap_or(&id).to_string();
            out.push(AppEntry { name, id });
        }
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out.dedup_by(|a, b| a.id == b.id);
        out
    })
}

unsafe fn display(item: &IShellItem, kind: windows::Win32::UI::Shell::SIGDN) -> String {
    match unsafe { item.GetDisplayName(kind) } {
        Ok(p) => {
            let s = unsafe { p.to_string() }.unwrap_or_default();
            unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
            s
        }
        Err(_) => String::new(),
    }
}

pub fn app_path(aumid: &str) -> String {
    format!("shell:AppsFolder\\{aumid}")
}

/// Open a file, folder, URL or shell path with its default handler.
pub fn open(target: &str) {
    let target = target.to_string();
    call(move || unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(target), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    });
}

pub fn reveal(path: &str) {
    let _ = std::process::Command::new("explorer.exe").arg(format!("/select,{path}")).spawn();
}

/// Move to the Recycle Bin (never a permanent delete).
pub fn recycle(path: &str) -> bool {
    let mut from: Vec<u16> = path.encode_utf16().collect();
    from.extend([0, 0]); // double-null terminated list
    call(move || unsafe {
        let mut op = SHFILEOPSTRUCTW {
            wFunc: FO_DELETE,
            pFrom: PCWSTR(from.as_ptr()),
            fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16,
            ..Default::default()
        };
        SHFileOperationW(&mut op) == 0 && !op.fAnyOperationsAborted.as_bool()
    })
}
