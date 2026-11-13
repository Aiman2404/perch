mod audio;
mod brightness;
mod clipboard;
mod media;
mod settings;
mod shelf;
mod shell;
mod system;
mod tracker;

use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

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
    power: system::Power,
    /// Unix ms when the running timer ends, if any.
    timer_end: Option<i64>,
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
        power: system::power(),
        timer_end: *TIMER.lock().unwrap(),
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

/// If WebView2's page process dies (out of memory, a driver hiccup), reload instead of
/// leaving "This page is having a problem" stuck across the top of the screen.
/// If the whole browser process is gone, restart Perch.
fn recover_on_crash(win: &WebviewWindow) {
    let app = win.app_handle().clone();
    let _ = win.with_webview(move |wv| unsafe {
        use webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED;
        use webview2_com::ProcessFailedEventHandler;
        let Ok(core) = wv.controller().CoreWebView2() else { return };
        let handler = ProcessFailedEventHandler::create(Box::new(move |sender, args| {
            let browser_gone = args.is_some_and(|a| {
                let mut kind = Default::default();
                a.ProcessFailedKind(&mut kind).is_ok() && kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED
            });
            if browser_gone {
                app.restart();
            } else if let Some(s) = sender {
                let _ = s.Reload();
            }
            Ok(())
        }));
        let mut token = 0i64;
        let _ = core.add_ProcessFailed(&handler, &mut token);
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
fn settings_get(store: State<Store>) -> settings::Settings {
    store.get()
}

#[tauri::command]
fn settings_patch(
    app: AppHandle,
    store: State<Store>,
    hist: State<History>,
    shared: State<Arc<tracker::Shared>>,
    patch: Value,
) -> settings::Settings {
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
    apply_enabled(&app, &shared, s.enabled);
    // Both windows listen, so a change in one shows up in the other immediately.
    let _ = app.emit("settings", &s);
    s
}

/// The master switch: hide the island and stop tracking, or bring it back.
fn apply_enabled(app: &AppHandle, shared: &tracker::Shared, on: bool) {
    shared.enabled.store(on, Ordering::Relaxed);
    if let Some(w) = app.get_webview_window("main") {
        let _ = if on { w.show() } else { w.hide() };
    }
    if let Some(item) = app.try_state::<ToggleItem>() {
        let _ = item.0.set_text(if on { "Turn Perch off" } else { "Turn Perch on" });
    }
}

fn set_enabled(app: &AppHandle, on: bool) {
    let s = app.state::<Store>().update(|s| s.enabled = on);
    apply_enabled(app, &app.state::<Arc<tracker::Shared>>(), on);
    let _ = app.emit("settings", &s);
}

struct ToggleItem(MenuItem<tauri::Wry>);

// ---------- the Perch window ----------

/// Opened from the tray, the Start menu, or by launching Perch while it's already running.
fn open_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    // Every window must share one WebView2 environment, so reuse the island's browser arguments.
    let args = app
        .config()
        .app
        .windows
        .first()
        .and_then(|w| w.additional_browser_args.clone())
        .unwrap_or_default();
    // Build it off the calling thread: creating a webview from a synchronous command (or
    // anything else running on the event loop) deadlocks on Windows and freezes the island.
    let app = app.clone();
    std::thread::spawn(move || build_settings(&app, &args));
}

fn build_settings(app: &AppHandle, args: &str) {
    if app.get_webview_window("settings").is_some() {
        return;
    }
    let built = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings.html".into()))
        .title("Perch")
        .inner_size(780.0, 540.0)
        .min_inner_size(680.0, 460.0)
        .decorations(false)
        .shadow(true)
        .background_color(tauri::window::Color(14, 14, 16, 255))
        .center()
        .additional_browser_args(args)
        .build();
    // The window's page shares the island's page process. Once it's closed, let the island
    // hand that memory back with its usual refresh-at-rest.
    if let Ok(w) = built {
        let app = app.clone();
        w.on_window_event(move |e| {
            if let tauri::WindowEvent::Destroyed = e {
                let _ = app.emit_to("main", "reclaim", ());
            }
        });
    }
}

#[tauri::command]
async fn show_settings(app: AppHandle) {
    open_settings(&app);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    version: String,
    memory_mb: f64,
    autostart: bool,
}

#[tauri::command]
async fn status(app: AppHandle) -> Status {
    Status {
        version: app.package_info().version.to_string(),
        memory_mb: system::memory_mb(),
        autostart: system::autostart(),
    }
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    shelf::clean_temp();
    app.exit(0);
}

#[tauri::command]
fn autostart_set(on: bool) -> bool {
    system::set_autostart(on)
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

// ---------- updates ----------
//
// Perch checks GitHub for a newer release shortly after starting and then every few hours.
// Releases carry a latest.json and a signature; the updater refuses anything not signed
// with Perch's own key. Nothing installs without you clicking Install.

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct UpdateStatus {
    current: String,
    available: Option<String>,
    notes: Option<String>,
    checking: bool,
    error: Option<String>,
}

static UPDATE: Mutex<Option<tauri_plugin_updater::Update>> = Mutex::new(None);
static UPDATE_STATUS: Mutex<Option<UpdateStatus>> = Mutex::new(None);

fn update_status_now(app: &AppHandle) -> UpdateStatus {
    UPDATE_STATUS.lock().unwrap().clone().unwrap_or_else(|| UpdateStatus {
        current: app.package_info().version.to_string(),
        ..Default::default()
    })
}

fn set_update_status(app: &AppHandle, f: impl FnOnce(&mut UpdateStatus)) -> UpdateStatus {
    let mut st = update_status_now(app);
    f(&mut st);
    *UPDATE_STATUS.lock().unwrap() = Some(st.clone());
    let _ = app.emit("update", &st);
    if let Some(item) = app.try_state::<UpdateItem>() {
        let _ = item.0.set_text(match &st.available {
            Some(v) => format!("Install update {v}"),
            None => "Check for updates".into(),
        });
    }
    st
}

struct UpdateItem(MenuItem<tauri::Wry>);

async fn check_for_update(app: &AppHandle) -> UpdateStatus {
    use tauri_plugin_updater::UpdaterExt;
    set_update_status(app, |s| s.checking = true);
    let res = match app.updater() {
        Ok(u) => u.check().await.map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    match res {
        Ok(Some(up)) => {
            let (v, notes) = (up.version.clone(), up.body.clone());
            *UPDATE.lock().unwrap() = Some(up);
            set_update_status(app, |s| {
                s.checking = false;
                s.error = None;
                s.available = Some(v);
                s.notes = notes;
            })
        }
        Ok(None) => set_update_status(app, |s| {
            s.checking = false;
            s.error = None;
            s.available = None;
        }),
        Err(e) => set_update_status(app, |s| {
            s.checking = false;
            s.error = Some(e);
        }),
    }
}

async fn install_update(app: &AppHandle) -> Result<(), String> {
    let up = UPDATE.lock().unwrap().clone().ok_or("No update available")?;
    shelf::clean_temp();
    up.download_and_install(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
    // On Windows the installer usually takes over before we get here.
    app.restart();
}

#[tauri::command]
fn update_status(app: AppHandle) -> UpdateStatus {
    update_status_now(&app)
}

#[tauri::command]
async fn update_check(app: AppHandle) -> UpdateStatus {
    check_for_update(&app).await
}

#[tauri::command]
async fn update_install(app: AppHandle) -> Result<(), String> {
    install_update(&app).await
}

fn spawn_update_checks(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio_sleep(Duration::from_secs(20)).await;
        loop {
            check_for_update(&app).await;
            tokio_sleep(Duration::from_secs(6 * 3600)).await;
        }
    });
}

/// A sleep that doesn't need tokio as a direct dependency.
async fn tokio_sleep(d: Duration) {
    let _ = tauri::async_runtime::spawn_blocking(move || std::thread::sleep(d)).await;
}

// ---------- sound ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Sound {
    master: audio::Volume,
    apps: Vec<audio::AppVolume>,
    devices: Vec<audio::Device>,
}

#[tauri::command]
async fn sound_state() -> Sound {
    Sound { master: audio::get(), apps: audio::apps(), devices: audio::devices() }
}

#[tauri::command]
async fn app_volume_set(key: String, level: f32) {
    audio::set_app(&key, level);
}

#[tauri::command]
async fn device_set(id: String) -> bool {
    audio::set_default_device(&id)
}

// ---------- quick ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Quick {
    wifi: Option<bool>,
    bluetooth: Option<bool>,
    mic_muted: Option<bool>,
    dark: bool,
    power: system::Power,
}

#[tauri::command]
async fn quick_state() -> Quick {
    let r = system::radios();
    Quick {
        wifi: r.wifi,
        bluetooth: r.bluetooth,
        mic_muted: audio::mic_muted(),
        dark: system::dark_mode(),
        power: system::power(),
    }
}

#[tauri::command]
async fn quick_toggle(which: String, on: bool) -> bool {
    match which.as_str() {
        "wifi" | "bluetooth" => system::set_radio(&which, on),
        "mic" => audio::set_mic_muted(on) == Some(on),
        "dark" => system::set_dark_mode(on),
        _ => false,
    }
}

static TIMER: Mutex<Option<i64>> = Mutex::new(None);

/// Start (secs) or cancel (None) the timer. Lives in Rust so it survives page refreshes.
#[tauri::command]
fn timer_set(app: AppHandle, secs: Option<u32>) -> Option<i64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let end = secs.map(|s| now + s as i64 * 1000);
    *TIMER.lock().unwrap() = end;
    let _ = app.emit("timer", end);
    end
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
        // Launching Perch while it's running opens its window instead of a second island.
        .plugin(tauri_plugin_single_instance::init(|app, _, _| open_settings(app)))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            shelf::clean_temp();
            let dir = app.path().app_config_dir().unwrap_or_else(|_| std::env::temp_dir().join("Perch"));
            let _ = ICON_DIR.set(dir.join("icons"));
            let store: Store = Arc::new(settings::Store::load(dir));
            let enabled = store.get().enabled;
            let shared = Arc::new(tracker::Shared::new(enabled));
            app.manage(shared.clone());
            app.manage(media::spawn(app.handle().clone()));
            app.manage(clipboard::spawn(app.handle().clone(), store.clone()));
            app.manage::<Shelf>(Arc::new(shelf::Shelf::default()));
            app.manage(store);

            let open = MenuItem::with_id(app, "open", "Open Perch", true, None::<&str>)?;
            let label = if enabled { "Turn Perch off" } else { "Turn Perch on" };
            let toggle = MenuItem::with_id(app, "toggle", label, true, None::<&str>)?;
            let upd = MenuItem::with_id(app, "update", "Check for updates", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Perch", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &toggle, &upd, &sep, &quit])?;
            app.manage(ToggleItem(toggle));
            app.manage(UpdateItem(upd));
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Perch")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, e| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = e
                    {
                        open_settings(tray.app_handle());
                    }
                })
                .on_menu_event(|app, e| match e.id.as_ref() {
                    "open" => open_settings(app),
                    "toggle" => {
                        let on = !app.state::<Store>().get().enabled;
                        set_enabled(app, on);
                    }
                    "update" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            if UPDATE.lock().unwrap().is_some() {
                                let _ = install_update(&app).await;
                            } else if check_for_update(&app).await.available.is_none() {
                                open_settings(&app);
                            }
                        });
                    }
                    "quit" => {
                        shelf::clean_temp();
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            let win = app.get_webview_window("main").unwrap();
            recover_on_crash(&win);
            if !enabled {
                let _ = win.hide();
            }
            tracker::spawn(win, shared);
            spawn_update_checks(app.handle().clone());
            if std::env::args().any(|a| a == "--settings") {
                open_settings(app.handle());
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            set_hit_rect,
            ui_ready,
            set_resting,
            set_typing,
            settings_get,
            settings_patch,
            show_settings,
            status,
            autostart_set,
            quit_app,
            update_status,
            update_check,
            update_install,
            sound_state,
            app_volume_set,
            device_set,
            quick_state,
            quick_toggle,
            timer_set,
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
