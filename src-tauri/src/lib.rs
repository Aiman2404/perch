mod audio;
mod media;
mod tracker;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, State};

/// Frontend reports the currently interactive rectangle (logical px, window-relative).
#[tauri::command]
fn set_hit_rect(shared: State<Arc<tracker::Shared>>, x: f64, y: f64, w: f64, h: f64) {
    *shared.hit.lock().unwrap() = tracker::Rect { x, y, w, h };
}

/// Called once the UI has loaded: re-emit everything it needs.
#[tauri::command]
fn ui_ready(shared: State<Arc<tracker::Shared>>, media: State<media::Handle>) -> bool {
    media.send(media::Msg::Refresh);
    shared.fullscreen.load(Ordering::Relaxed)
}

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
fn volume_get() -> audio::Volume {
    audio::get()
}

#[tauri::command]
fn volume_step(delta: f32) -> audio::Volume {
    audio::step(delta)
}

#[tauri::command]
fn volume_set(level: f32) -> audio::Volume {
    audio::set(level)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let shared = Arc::new(tracker::Shared::default());
            app.manage(shared.clone());
            app.manage(media::spawn(app.handle().clone()));

            let quit = MenuItem::with_id(app, "quit", "Quit Perch", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&quit])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Perch")
                .menu(&menu)
                .on_menu_event(|app, e| {
                    if e.id.as_ref() == "quit" {
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
            media_cmd,
            volume_get,
            volume_step,
            volume_set
        ])
        .run(tauri::generate_context!())
        .expect("error while running Perch");
}
