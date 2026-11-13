//! Core Audio: master volume, per-app volume, output devices and mic mute.

use crate::shell;
use serde::Serialize;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    AudioSessionStateExpired, DEVICE_STATE_ACTIVE, EDataFlow, IAudioSessionControl2, IAudioSessionManager2,
    IMMDevice, IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator, eCapture, eCommunications,
    eConsole, eMultimedia, eRender,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, STGM_READ,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface, PCWSTR, PWSTR, Result, interface};

#[derive(Serialize, Default)]
pub struct Volume {
    pub level: f32,
    pub muted: bool,
}

fn com() {
    // Tauri runs commands on various threads; make sure COM is up on this one.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

fn enumerator() -> Result<IMMDeviceEnumerator> {
    com();
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

fn default_device(flow: EDataFlow) -> Result<IMMDevice> {
    unsafe { enumerator()?.GetDefaultAudioEndpoint(flow, eConsole) }
}

fn endpoint(flow: EDataFlow) -> Result<IAudioEndpointVolume> {
    unsafe { default_device(flow)?.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) }
}

fn read(ep: &IAudioEndpointVolume) -> Result<Volume> {
    unsafe {
        Ok(Volume {
            level: ep.GetMasterVolumeLevelScalar()?,
            muted: ep.GetMute()?.as_bool(),
        })
    }
}

// ---------- master ----------

pub fn get() -> Volume {
    endpoint(eRender).and_then(|ep| read(&ep)).unwrap_or_default()
}

pub fn set(level: f32) -> Volume {
    let level = level.clamp(0.0, 1.0);
    endpoint(eRender)
        .and_then(|ep| unsafe {
            ep.SetMasterVolumeLevelScalar(level, &GUID::zeroed())?;
            if level > 0.0 && ep.GetMute()?.as_bool() {
                ep.SetMute(false, &GUID::zeroed())?;
            }
            read(&ep)
        })
        .unwrap_or_default()
}

// ---------- microphone ----------

pub fn mic_muted() -> Option<bool> {
    endpoint(eCapture).ok().and_then(|ep| unsafe { ep.GetMute().ok() }).map(|b| b.as_bool())
}

pub fn set_mic_muted(muted: bool) -> Option<bool> {
    let ep = endpoint(eCapture).ok()?;
    unsafe {
        ep.SetMute(muted, &GUID::zeroed()).ok()?;
    }
    mic_muted()
}

// ---------- per-app volume ----------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AppVolume {
    /// Grouping key: lowercase exe name ("spotify.exe"), or "system" for system sounds.
    pub key: String,
    pub name: String,
    pub level: f32,
    pub muted: bool,
    pub active: bool,
    pub icon: Option<String>,
}

static ICONS: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);

fn exe_path(pid: u32) -> Option<String> {
    unsafe {
        let h: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        ok.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn pretty(exe: &str) -> String {
    let stem = exe.trim_end_matches(".exe");
    let known = [
        ("msedge", "Edge"), ("chrome", "Chrome"), ("firefox", "Firefox"), ("spotify", "Spotify"),
        ("discord", "Discord"), ("brave", "Brave"), ("opera", "Opera"), ("vlc", "VLC"), ("teams", "Teams"),
        ("ms-teams", "Teams"), ("zoom", "Zoom"), ("steam", "Steam"), ("applemusic", "Apple Music"),
        ("whatsapp", "WhatsApp"), ("telegram", "Telegram"), ("slack", "Slack"),
    ];
    for (k, v) in known {
        if stem == k {
            return v.into();
        }
    }
    let mut c = stem.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// Every app currently holding an audio session on the default output.
fn for_each_session(mut f: impl FnMut(&str, Option<String>, &ISimpleAudioVolume, bool, bool)) -> Result<()> {
    unsafe {
        let dev = default_device(eRender)?;
        let mgr: IAudioSessionManager2 = dev.Activate(CLSCTX_ALL, None)?;
        let list = mgr.GetSessionEnumerator()?;
        for i in 0..list.GetCount()? {
            let Ok(ctl) = list.GetSession(i) else { continue };
            let Ok(state) = ctl.GetState() else { continue };
            if state == AudioSessionStateExpired {
                continue;
            }
            let Ok(c2) = ctl.cast::<IAudioSessionControl2>() else { continue };
            let Ok(vol) = ctl.cast::<ISimpleAudioVolume>() else { continue };
            let active = state.0 == 1; // AudioSessionStateActive
            if c2.IsSystemSoundsSession() == windows::Win32::Foundation::S_OK {
                f("system", None, &vol, active, true);
                continue;
            }
            let pid = c2.GetProcessId().unwrap_or(0);
            if pid == 0 {
                continue;
            }
            let Some(path) = exe_path(pid) else { continue };
            let exe = path.rsplit('\\').next().unwrap_or(&path).to_lowercase();
            if exe == "perch.exe" || exe == "msedgewebview2.exe" {
                continue;
            }
            f(&exe, Some(path), &vol, active, false);
        }
    }
    Ok(())
}

pub fn apps() -> Vec<AppVolume> {
    let mut out: Vec<AppVolume> = Vec::new();
    let mut paths: Vec<(String, String)> = Vec::new();
    let _ = for_each_session(|key, path, vol, active, system| unsafe {
        let level = vol.GetMasterVolume().unwrap_or(1.0);
        let muted = vol.GetMute().map(|b| b.as_bool()).unwrap_or(false);
        if let Some(a) = out.iter_mut().find(|a| a.key == key) {
            a.active |= active;
            return;
        }
        if let Some(p) = path {
            paths.push((key.to_string(), p));
        }
        out.push(AppVolume {
            key: key.to_string(),
            name: if system { "System sounds".into() } else { pretty(key) },
            level,
            muted,
            active,
            icon: None,
        });
    });

    // Icons come from the exe, cached for the life of the process.
    let mut cache = ICONS.lock().unwrap();
    let cache = cache.get_or_insert_with(HashMap::new);
    for a in out.iter_mut() {
        if let Some((_, p)) = paths.iter().find(|(k, _)| *k == a.key) {
            let icon = cache
                .entry(a.key.clone())
                .or_insert_with(|| shell::image_png(p, 48, true).as_deref().map(shell::data_url));
            a.icon = icon.clone();
        }
    }
    // Playing apps first, system sounds last.
    out.sort_by_key(|a| (a.key == "system", !a.active, a.name.to_lowercase()));
    out
}

pub fn set_app(key: &str, level: f32) {
    let level = level.clamp(0.0, 1.0);
    let _ = for_each_session(|k, _, vol, _, _| unsafe {
        if k == key {
            let _ = vol.SetMasterVolume(level, &GUID::zeroed());
            if level > 0.0 {
                let _ = vol.SetMute(false, &GUID::zeroed());
            }
        }
    });
}

// ---------- output devices ----------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
    pub default: bool,
}

unsafe fn pwstr(p: PWSTR) -> String {
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
    s
}

pub fn devices() -> Vec<Device> {
    let run = || -> Result<Vec<Device>> {
        unsafe {
            let en = enumerator()?;
            let def = en
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .and_then(|d| d.GetId())
                .map(|p| pwstr(p))
                .unwrap_or_default();
            let col = en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            let mut out = Vec::new();
            for i in 0..col.GetCount()? {
                let dev = col.Item(i)?;
                let id = pwstr(dev.GetId()?);
                let props = dev.OpenPropertyStore(STGM_READ)?;
                let v = props.GetValue(&PKEY_Device_FriendlyName)?;
                let name = PropVariantToStringAlloc(&v).map(|p| pwstr(p)).unwrap_or_default();
                out.push(Device { default: id == def, id, name });
            }
            Ok(out)
        }
    };
    run().unwrap_or_default()
}

#[allow(non_snake_case)]
mod policy {
    use super::*;
/// Undocumented but long-stable interface Windows' own Sound settings use to switch the default device.
#[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
pub unsafe trait IPolicyConfig: IUnknown {
    fn GetMixFormat(&self, id: PCWSTR, fmt: *mut *mut c_void) -> HRESULT;
    fn GetDeviceFormat(&self, id: PCWSTR, default: i32, fmt: *mut *mut c_void) -> HRESULT;
    fn ResetDeviceFormat(&self, id: PCWSTR) -> HRESULT;
    fn SetDeviceFormat(&self, id: PCWSTR, endpoint: *mut c_void, mix: *mut c_void) -> HRESULT;
    fn GetProcessingPeriod(&self, id: PCWSTR, default: i32, a: *mut i64, b: *mut i64) -> HRESULT;
    fn SetProcessingPeriod(&self, id: PCWSTR, a: *mut i64) -> HRESULT;
    fn GetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
    fn SetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
    fn GetPropertyValue(&self, id: PCWSTR, key: *const c_void, v: *mut c_void) -> HRESULT;
    fn SetPropertyValue(&self, id: PCWSTR, key: *const c_void, v: *mut c_void) -> HRESULT;
    fn SetDefaultEndpoint(&self, id: PCWSTR, role: u32) -> HRESULT;
    fn SetEndpointVisibility(&self, id: PCWSTR, visible: i32) -> HRESULT;
}

const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

pub fn set_default_device(id: &str) -> bool {
    com();
    let wide: Vec<u16> = id.encode_utf16().chain([0]).collect();
    unsafe {
        let Ok(pc) = CoCreateInstance::<_, IPolicyConfig>(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL) else {
            return false;
        };
        [eConsole, eMultimedia, eCommunications]
            .iter()
            .all(|r| pc.SetDefaultEndpoint(PCWSTR(wide.as_ptr()), r.0 as u32).is_ok())
    }
}
}
pub use policy::set_default_device;

