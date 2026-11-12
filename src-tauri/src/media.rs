//! Whatever Windows is playing (Spotify, a YouTube tab, VLC, …) via the
//! System Media Transport Controls. Runs on its own thread; WinRT events just
//! poke a channel, and the thread re-reads state and emits only on change.

use base64::Engine;
use serde::Serialize;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::Result;

pub enum Cmd {
    Toggle,
    Next,
    Prev,
    Seek(f64),
}

pub enum Msg {
    Changed,
    Refresh,
    Cmd(Cmd),
}

pub struct Handle(Sender<Msg>);

impl Handle {
    pub fn send(&self, m: Msg) {
        let _ = self.0.send(m);
    }
}

#[derive(Serialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
struct MediaState {
    has: bool,
    title: String,
    artist: String,
    album: String,
    app: String,
    playing: bool,
    can_prev: bool,
    can_next: bool,
    /// Seconds into the track at `sampled_at`.
    position: f64,
    duration: f64,
    /// Unix ms when `position` was true; the UI extrapolates from here.
    sampled_at: i64,
}

struct Hooks {
    session: Session,
    props: i64,
    playback: i64,
    timeline: i64,
}

impl Drop for Hooks {
    fn drop(&mut self) {
        let _ = self.session.RemoveMediaPropertiesChanged(self.props);
        let _ = self.session.RemovePlaybackInfoChanged(self.playback);
        let _ = self.session.RemoveTimelinePropertiesChanged(self.timeline);
    }
}

const TICKS_PER_SEC: f64 = 10_000_000.0;
const FILETIME_UNIX_OFFSET: i64 = 116_444_736_000_000_000;

pub fn spawn(app: AppHandle) -> Handle {
    let (tx, rx) = mpsc::channel::<Msg>();
    let handle = Handle(tx.clone());

    thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let mgr = match Manager::RequestAsync().and_then(|op| op.join()) {
            Ok(m) => m,
            Err(_) => return,
        };
        // Manager lives for the whole process, so its tokens are never removed.
        let poke = || {
            let tx = tx.clone();
            move |_: windows::core::Ref<Manager>, _: windows::core::Ref<_>| {
                let _ = tx.send(Msg::Changed);
                Ok(())
            }
        };
        let _ = mgr.CurrentSessionChanged(&TypedEventHandler::new(poke()));
        let _ = mgr.SessionsChanged(&TypedEventHandler::new({
            let tx = tx.clone();
            move |_, _| {
                let _ = tx.send(Msg::Changed);
                Ok(())
            }
        }));

        let mut hooks: Option<Hooks> = None;
        let mut last = MediaState::default();
        let mut last_key = String::new();

        loop {
            let mut force = false;
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(Msg::Cmd(c)) => {
                    if let Some(h) = &hooks {
                        run_cmd(&h.session, c);
                    }
                }
                Ok(Msg::Refresh) => force = true,
                Ok(Msg::Changed) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }

            let session = pick_session(&mgr);
            let same = match (&hooks, &session) {
                (Some(h), Some(s)) => h.session == *s,
                (None, None) => true,
                _ => false,
            };
            if !same {
                hooks = session.as_ref().and_then(|s| attach(s, &tx).ok());
            }

            let (state, thumb) = match &session {
                Some(s) => read_state(s).unwrap_or_default(),
                None => (MediaState::default(), None),
            };

            // Art is sent separately and only when the track (or its art) changes.
            let key = format!("{}\u{1}{}\u{1}{}", state.title, state.artist, thumb.is_some());
            if force || key != last_key {
                last_key = key;
                let art = thumb.and_then(|t| read_thumb(&t).ok());
                let _ = app.emit("media-art", art);
            }
            if force || state != last {
                let _ = app.emit("media", &state);
                last = state;
            }
        }
    });

    handle
}

/// Prefer a session that's actually playing; fall back to Windows' "current" one.
fn pick_session(mgr: &Manager) -> Option<Session> {
    if let Ok(list) = mgr.GetSessions() {
        for s in list {
            let playing = s
                .GetPlaybackInfo()
                .and_then(|i| i.PlaybackStatus())
                .map(|st| st == Status::Playing)
                .unwrap_or(false);
            if playing {
                return Some(s);
            }
        }
    }
    mgr.GetCurrentSession().ok()
}

fn attach(s: &Session, tx: &Sender<Msg>) -> Result<Hooks> {
    // Each event has different arg types, so build a fresh closure per event.
    macro_rules! poke {
        () => {{
            let tx = tx.clone();
            TypedEventHandler::new(move |_, _| {
                let _ = tx.send(Msg::Changed);
                Ok(())
            })
        }};
    }
    Ok(Hooks {
        props: s.MediaPropertiesChanged(&poke!())?,
        playback: s.PlaybackInfoChanged(&poke!())?,
        timeline: s.TimelinePropertiesChanged(&poke!())?,
        session: s.clone(),
    })
}

fn read_state(s: &Session) -> Result<(MediaState, Option<IRandomAccessStreamReference>)> {
    let props = s.TryGetMediaPropertiesAsync()?.join()?;
    let info = s.GetPlaybackInfo()?;
    let controls = info.Controls()?;
    let tl = s.GetTimelineProperties()?;

    let start = tl.StartTime()?.Duration;
    let end = tl.EndTime()?.Duration;
    let pos = tl.Position()?.Duration;
    let updated = tl.LastUpdatedTime()?.UniversalTime;
    let sampled_at = if updated > 0 {
        (updated - FILETIME_UNIX_OFFSET) / 10_000
    } else {
        now_ms()
    };

    let state = MediaState {
        has: true,
        title: props.Title()?.to_string(),
        artist: props.Artist()?.to_string(),
        album: props.AlbumTitle()?.to_string(),
        app: friendly_app(&s.SourceAppUserModelId()?.to_string()),
        playing: info.PlaybackStatus()? == Status::Playing,
        can_prev: controls.IsPreviousEnabled()?,
        can_next: controls.IsNextEnabled()?,
        position: ((pos - start).max(0)) as f64 / TICKS_PER_SEC,
        duration: ((end - start).max(0)) as f64 / TICKS_PER_SEC,
        sampled_at,
    };
    Ok((state, props.Thumbnail().ok()))
}

fn read_thumb(r: &IRandomAccessStreamReference) -> Result<String> {
    let stream = r.OpenReadAsync()?.join()?;
    let size = stream.Size()? as u32;
    let reader = DataReader::CreateDataReader(&stream)?;
    reader.LoadAsync(size)?.join()?;
    let mut buf = vec![0u8; size as usize];
    reader.ReadBytes(&mut buf)?;
    let ct = stream.ContentType().map(|c| c.to_string()).unwrap_or_default();
    let ct = if ct.starts_with("image/") { ct } else { "image/png".into() };
    Ok(format!(
        "data:{ct};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(buf)
    ))
}

fn run_cmd(s: &Session, c: Cmd) {
    let _ = match c {
        Cmd::Toggle => s.TryTogglePlayPauseAsync().and_then(|o| o.join()),
        Cmd::Next => s.TrySkipNextAsync().and_then(|o| o.join()),
        Cmd::Prev => s.TrySkipPreviousAsync().and_then(|o| o.join()),
        Cmd::Seek(secs) => {
            let start = s
                .GetTimelineProperties()
                .and_then(|t| t.StartTime())
                .map(|t| t.Duration)
                .unwrap_or(0);
            s.TryChangePlaybackPositionAsync(start + (secs * TICKS_PER_SEC) as i64)
                .and_then(|o| o.join())
        }
    };
}

fn friendly_app(aumid: &str) -> String {
    let id = aumid.to_ascii_lowercase();
    let known = [
        ("spotify", "Spotify"),
        ("chrome", "Chrome"),
        ("msedge", "Edge"),
        ("firefox", "Firefox"),
        ("brave", "Brave"),
        ("opera", "Opera"),
        ("vivaldi", "Vivaldi"),
        ("zune", "Media Player"),
        ("vlc", "VLC"),
        ("applemusic", "Apple Music"),
        ("tidal", "TIDAL"),
        ("deezer", "Deezer"),
        ("discord", "Discord"),
    ];
    for (needle, name) in known {
        if id.contains(needle) {
            return name.into();
        }
    }
    let base = aumid.rsplit(['!', '\\']).next().unwrap_or(aumid);
    base.trim_end_matches(".exe").trim_end_matches(".EXE").to_string()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
