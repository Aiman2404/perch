mod audio;
mod brightness;
mod clipboard;
mod media;
mod settings;
mod shelf;
mod shell;
mod tracker;

use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

type Shelf = Arc<shelf::Shelf>;
type History = Arc<clipboard::History>;
type Store = Arc<settings::Store>;

// ---------- window / lifecycle ----------

/// Frontend reports the currently interactive rectangle (logical px, window-relative).
#[tauri::command]
fn set_hit_rect(shared: State<Arc<tracker::Shared>>, x: f64, y: f64, w: f64, h: f64) {
    *shared.hit.lock().unwrap() = tracker::Rect { x, y, w, h };
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Boot {
    fullscreen: bool,
    hovered: bool,
    settings: settings::Settings,
    /// Windows' default touchpad setting: fingers up scrolls content down.
    touchpad_natural: bool,
    shelf: Vec<shelf::Item>,
    clipboard: Vec<clipboard::View>,
}

/// Called once the UI has loaded: hand it everything it needs.
#[tauri::command]
fn ui_ready(
    shared: State<Arc<tracker::Shared>>,
    media: State<media::Handle>,
    store: State<Store>,
    shelf: State<Shelf>,
    hist: State<History>,
) -> Boot {
    media.send(media::Msg::Refresh);
    Boot {
        fullscreen: shared.fullscreen.load(Ordering::Relaxed),
        hovered: shared.hovered.load(Ordering::Relaxed),
        settings: store.get(),
        touchpad_natural: touchpad_natural(),
        shelf: shelf.list(),
        clipboard: hist.views(),
    }
}

fn touchpad_natural() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::w;
    let mut v: u32 = 0;
    let mut size = 4u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\PrecisionTouchPad"),
            w!("ScrollDirection"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    // Missing value or 0 = Windows' default ("down motion scrolls up").
    ok.is_err() || v == 0
}

/// While tucked away, ask WebView2 to drop caches and trim its working set.
#[tauri::command]
fn set_resting(win: WebviewWindow, resting: bool) {
    let _ = win.with_webview(move |wv| unsafe {
        use webview2_com::Microsoft::Web::WebView2::Win32::{
            COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
            ICoreWebView2_19,
        };
        use windows::core::Interface;
        if let Ok(core) = wv.controller().CoreWebView2() {
            if let Ok(c) = core.cast::<ICoreWebView2_19>() {
                let _ = c.SetMemoryUsageTargetLevel(if resting {
                    COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
                } else {
                    COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
                });
            }
        }
    });
}

/// The island never takes focus, except while you type in the app search.
#[tauri::command]
fn set_typing(win: WebviewWindow, on: bool) {
    let _ = win.set_focusable(on);
    if on {
        let _ = win.set_focus();
    }
}

// ---------- settings ----------

#[tauri::command]
fn settings_patch(store: State<Store>, hist: State<History>, patch: Value) -> settings::Settings {
    let s = store.update(|s| {
        let mut v = serde_json::to_value(&*s).unwrap_or(Value::Null);
        if let (Some(obj), Some(p)) = (v.as_object_mut(), patch.as_object()) {
            for (k, val) in p {
                obj.insert(k.clone(), val.clone());
            }
        }
        if let Ok(n) = serde_json::from_value(v) {
            *s = n;
        }
    });
    hist.trim(s.clipboard_size);
    s
}

// ---------- media ----------

#[tauri::command]
fn media_cmd(media: State<media::Handle>, action: String, value: Option<f64>) {
    let cmd = match action.as_str() {
        "toggle" => media::Cmd::Toggle,
        "next" => media::Cmd::Next,
        "prev" => media::Cmd::Prev,
        "seek" => media::Cmd::Seek(value.unwrap_or(0.0)),
        _ => return,
    };
    media.send(media::Msg::Cmd(cmd));
}

#[tauri::command]
fn media_select(media: State<media::Handle>, id: String) {
    media.send(media::Msg::Select(id));
}

// ---------- volume / brightness ----------

#[tauri::command]
fn volume_get() -> audio::Volume {
    audio::get()
}

#[tauri::command]
fn volume_set(level: f32) -> audio::Volume {
    audio::set(level)
}

#[tauri::command]
async fn brightness_get() -> Option<u8> {
    brightness::get()
}

#[tauri::command]
fn brightness_set(level: u8) {
    brightness::set(level);
}

// ---------- apps ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppView {
    name: String,
    target: String,
    kind: String,
    icon: Option<String>,
}

static APPS: Mutex<Option<(Instant, Vec<shell::AppEntry>)>> = Mutex::new(None);

fn all_apps() -> Vec<shell::AppEntry> {
    let mut cache = APPS.lock().unwrap();
    if let Some((at, list)) = &*cache {
        if at.elapsed() < Duration::from_secs(120) {
            return list.clone();
        }
    }
    let list = shell::list_apps();
    *cache = Some((Instant::now(), list.clone()));
    list
}

static ICON_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// App icons are cached on disk so a normal start never has to load the shell's icon machinery.
fn app_icon(aumid: &str) -> Option<Vec<u8>> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    aumid.hash(&mut h);
    let file = ICON_DIR.get().map(|d| d.join(format!("{:016x}.png", h.finish())));
    if let Some(bytes) = file.as_ref().and_then(|f| std::fs::read(f).ok()) {
        return Some(bytes);
    }
    let png = shell::image_png(&shell::app_path(aumid), 64, true)?;
    if let Some(f) = file {
        let _ = std::fs::create_dir_all(f.parent().unwrap());
        let _ = std::fs::write(f, &png);
    }
    Some(png)
}

fn view_of(a: &settings::QuickApp) -> AppView {
    let icon = if a.kind == "app" { app_icon(&a.target).as_deref().map(shell::data_url) } else { None };
    AppView { name: a.name.clone(), target: a.target.clone(), kind: a.kind.clone(), icon }
}

/// Saved quick apps (seeded on first run with whatever of these exist).
#[tauri::command]
async fn quick_apps(store: State<'_, Store>) -> Result<Vec<AppView>, ()> {
    let s = store.get();
    let list = match s.quick_apps {
        Some(l) => l,
        None => {
            let apps = all_apps();
            let mut seed = vec![settings::QuickApp {
                name: "YouTube".into(),
                target: "https://www.youtube.com".into(),
                kind: "url".into(),
            }];
            for want in ["Spotify", "File Explorer", "Settings"] {
                if let Some(a) = apps.iter().find(|a| a.name.eq_ignore_ascii_case(want)) {
                    seed.push(settings::QuickApp { name: a.name.clone(), target: a.id.clone(), kind: "app".into() });
                }
            }
            store.update(|s| s.quick_apps = Some(seed.clone()));
            seed
        }
    };
    Ok(list.iter().map(view_of).collect())
}

#[tauri::command]
async fn quick_apps_save(store: State<'_, Store>, apps: Vec<settings::QuickApp>) -> Result<Vec<AppView>, ()> {
    store.update(|s| s.quick_apps = Some(apps.clone()));
    Ok(apps.iter().map(view_of).collect())
}

/// Search installed apps; a typed address also offers itself as a website.
#[tauri::command]
async fn apps_search(query: String) -> Vec<AppView> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return vec![];
    }
    let mut hits: Vec<(u8, shell::AppEntry)> = all_apps()
        .into_iter()
        .filter_map(|a| {
            let n = a.name.to_lowercase();
            let rank = if n.starts_with(&q) {
                0
            } else if n.split_whitespace().any(|w| w.starts_with(&q)) {
                1
            } else if n.contains(&q) {
                2
            } else {
                return None;
            };
            Some((rank, a))
        })
        .collect();
    hits.sort_by_key(|(r, a)| (*r, a.name.len()));

    let mut out: Vec<AppView> = hits
        .into_iter()
        .take(5)
        .map(|(_, a)| view_of(&settings::QuickApp { name: a.name, target: a.id, kind: "app".into() }))
        .collect();
    if !q.contains(' ') && q.contains('.') {
        let url = if q.starts_with("http") { q.clone() } else { format!("https://{q}") };
        let name = q.trim_start_matches("https://").trim_start_matches("http://").trim_start_matches("www.");
        let name = name.split('.').next().unwrap_or(name);
        let mut c = name.chars();
        let name = c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default();
        out.insert(0, AppView { name, target: url, kind: "url".into(), icon: None });
        out.truncate(5);
    }
    out
}

#[tauri::command]
async fn quick_launch(target: String, kind: String) {
    let t = if kind == "app" { shell::app_path(&target) } else { target };
    shell::open(&t);
}

// ---------- shelf ----------

fn emit_shelf(app: &AppHandle, shelf: &Shelf) {
    let _ = app.emit("shelf", shelf.list());
}

#[tauri::command]
async fn shelf_add(app: AppHandle, shelf: State<'_, Shelf>, store: State<'_, Store>, paths: Vec<String>) -> Result<usize, ()> {
    let n = shelf.add(paths, store.get().shelf_mode == "copy");
    emit_shelf(&app, &shelf);
    Ok(n)
}

#[tauri::command]
async fn shelf_action(app: AppHandle, shelf: State<'_, Shelf>, id: u32, action: String) -> Result<bool, ()> {
    let Some(it) = shelf.get(id) else { return Ok(false) };
    let ok = match action.as_str() {
        "open" => {
            shell::open(&it.path);
            true
        }
        "reveal" => {
            shell::reveal(&it.source);
            true
        }
        "copy" => clipboard::set_files(&[it.path.as_str()]),
        "remove" => {
            shelf.remove(id);
            true
        }
        "trash" => shelf.trash(id),
        _ => false,
    };
    emit_shelf(&app, &shelf);
    Ok(ok)
}

#[tauri::command]
fn shelf_clear(app: AppHandle, shelf: State<Shelf>) {
    shelf.clear();
    emit_shelf(&app, &shelf);
}

/// Start a real OS drag of a shelf file, so it can be dropped anywhere.
#[tauri::command]
fn shelf_drag(app: AppHandle, win: WebviewWindow, shelf: State<Shelf>, id: u32) {
    let Some(it) = shelf.get(id) else { return };
    let done = app.clone();
    let _ = app.run_on_main_thread(move || {
        let _ = drag::start_drag(
            &win,
            drag::DragItem::Files(vec![PathBuf::from(&it.path)]),
            drag::Image::Raw(it.icon_png.unwrap_or_default()),
            move |_, _| {
                let _ = done.emit("drag-out-end", ());
            },
            drag::Options::default(),
        );
    });
}

// ---------- clipboard ----------

#[tauri::command]
async fn clip_copy(hist: State<'_, History>, id: u32) -> Result<bool, ()> {
    Ok(hist.copy(id))
}

#[tauri::command]
fn clip_remove(app: AppHandle, hist: State<History>, id: u32) {
    hist.remove(id);
    let _ = app.emit("clipboard", hist.views());
}

#[tauri::command]
fn clip_clear(app: AppHandle, hist: State<History>) {
    hist.clear();
    let _ = app.emit("clipboard", hist.views());
}

// ---------- boot ----------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            shelf::clean_temp();
            let dir = app.path().app_config_dir().unwrap_or_else(|_| std::env::temp_dir().join("Perch"));
            let _ = ICON_DIR.set(dir.join("icons"));
            let store: Store = Arc::new(settings::Store::load(dir));
            let shared = Arc::new(tracker::Shared::default());
            app.manage(shared.clone());
            app.manage(media::spawn(app.handle().clone()));
            app.manage(clipboard::spawn(app.handle().clone(), store.clone()));
            app.manage::<Shelf>(Arc::new(shelf::Shelf::default()));
            app.manage(store);

            let quit = MenuItem::with_id(app, "quit", "Quit Perch", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&quit])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Perch")
                .menu(&menu)
                .on_menu_event(|app, e| {
                    if e.id.as_ref() == "quit" {
                        shelf::clean_temp();
                        app.exit(0);
                    }
                })
                .build(app)?;

            let win = app.get_webview_window("main").unwrap();
            tracker::spawn(win, shared);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            set_hit_rect,
            ui_ready,
            set_resting,
            set_typing,
            settings_patch,
            media_cmd,
            media_select,
            volume_get,
            volume_set,
            brightness_get,
            brightness_set,
            quick_apps,
            quick_apps_save,
            apps_search,
            quick_launch,
            shelf_add,
            shelf_action,
            shelf_clear,
            shelf_drag,
            clip_copy,
            clip_remove,
            clip_clear
        ])
        .run(tauri::generate_context!())
        .expect("error while running Perch");
}
