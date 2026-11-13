//! Persistent preferences (the only thing Perch keeps across restarts).
//! Shelf and clipboard contents are deliberately memory-only.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct QuickApp {
    pub name: String,
    /// AppUserModelID for apps, URL for websites.
    pub target: String,
    /// "app" | "url"
    pub kind: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Master switch: when off the island is hidden and Perch just waits in the tray.
    pub enabled: bool,
    /// "obsidian" | "graphite" | "aurora"
    pub theme: String,
    /// Frosted glass made from the album cover behind the player.
    pub glass_player: bool,
    /// How long the island waits after the cursor leaves before tucking away.
    pub retract_ms: u32,
    /// While a game or video is fullscreen: "hold" | "hide" | "normal".
    pub fullscreen: String,
    /// Mouse-wheel volume change per notch, in percent.
    pub volume_step: u32,
    /// None until first run seeds a few sensible defaults.
    pub quick_apps: Option<Vec<QuickApp>>,
    /// "link" keeps a reference to the original file, "copy" parks a temporary copy.
    pub shelf_mode: String,
    pub clipboard_size: usize,
    pub clipboard_paused: bool,
    /// Flip if two-finger volume feels backwards.
    pub touchpad_invert: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            theme: "obsidian".into(),
            glass_player: true,
            retract_ms: 2500,
            fullscreen: "hold".into(),
            volume_step: 3,
            quick_apps: None,
            shelf_mode: "link".into(),
            clipboard_size: 5,
            clipboard_paused: false,
            touchpad_invert: false,
        }
    }
}

pub struct Store {
    path: PathBuf,
    pub data: Mutex<Settings>,
}

impl Store {
    pub fn load(dir: PathBuf) -> Self {
        let path = dir.join("settings.json");
        let data = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { path, data: Mutex::new(data) }
    }

    pub fn get(&self) -> Settings {
        self.data.lock().unwrap().clone()
    }

    pub fn update(&self, f: impl FnOnce(&mut Settings)) -> Settings {
        let mut d = self.data.lock().unwrap();
        f(&mut d);
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(&*d) {
            let _ = std::fs::write(&self.path, json);
        }
        d.clone()
    }
}
