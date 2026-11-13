//! The shelf: files dropped on the island. Memory-only, so it empties on
//! restart/shutdown but survives sleep. "link" mode keeps a reference to the
//! original; "copy" mode parks a temporary copy that is cleaned up later.

use crate::shell;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: u32,
    /// Where the file lives now (the original, or our temp copy).
    pub path: String,
    /// The file it came from.
    pub source: String,
    pub name: String,
    pub size: u64,
    pub dir: bool,
    pub copied: bool,
    pub icon: Option<String>,
    #[serde(skip)]
    pub icon_png: Option<Vec<u8>>,
}

#[derive(Default)]
pub struct Shelf {
    items: Mutex<Vec<Item>>,
    next: AtomicU32,
}

fn temp_root() -> PathBuf {
    std::env::temp_dir().join("Perch").join("shelf")
}

/// Leftovers from a previous session (crash, shutdown) are not kept.
pub fn clean_temp() {
    let _ = std::fs::remove_dir_all(temp_root());
}

fn copy_into(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            copy_into(&e.path(), &dst.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dst).map(|_| ())
    }
}

impl Shelf {
    pub fn list(&self) -> Vec<Item> {
        self.items.lock().unwrap().clone()
    }

    /// Returns how many new items were added.
    pub fn add(&self, paths: Vec<String>, copy: bool) -> usize {
        let mut added = 0;
        for p in paths {
            let src = PathBuf::from(&p);
            let Ok(meta) = std::fs::metadata(&src) else { continue };
            {
                let items = self.items.lock().unwrap();
                if items.iter().any(|i| i.source == p || i.path == p) {
                    continue; // already on the shelf (also covers dragging a tile back onto the island)
                }
            }
            let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
            let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(p.clone());
            let mut path = p.clone();
            let mut copied = false;
            if copy {
                let dst = temp_root().join(id.to_string()).join(&name);
                if std::fs::create_dir_all(dst.parent().unwrap()).is_ok() && copy_into(&src, &dst).is_ok() {
                    path = dst.to_string_lossy().into_owned();
                    copied = true;
                }
            }
            let icon_png = shell::image_png(&path, 96, false);
            self.items.lock().unwrap().push(Item {
                id,
                icon: icon_png.as_deref().map(shell::data_url),
                icon_png,
                path,
                source: p,
                name,
                size: if meta.is_dir() { 0 } else { meta.len() },
                dir: meta.is_dir(),
                copied,
            });
            added += 1;
        }
        added
    }

    pub fn get(&self, id: u32) -> Option<Item> {
        self.items.lock().unwrap().iter().find(|i| i.id == id).cloned()
    }

    /// Take it off the shelf. A temporary copy is deleted; an original is untouched.
    pub fn remove(&self, id: u32) {
        let mut items = self.items.lock().unwrap();
        if let Some(pos) = items.iter().position(|i| i.id == id) {
            let it = items.remove(pos);
            if it.copied {
                let _ = std::fs::remove_dir_all(temp_root().join(it.id.to_string()));
            }
        }
    }

    /// Move the original file to the Recycle Bin and take it off the shelf.
    pub fn trash(&self, id: u32) -> bool {
        let Some(it) = self.get(id) else { return false };
        let ok = shell::recycle(&it.source);
        if ok {
            self.remove(id);
        }
        ok
    }

    pub fn clear(&self) {
        let ids: Vec<u32> = self.list().iter().map(|i| i.id).collect();
        for id in ids {
            self.remove(id);
        }
    }
}
