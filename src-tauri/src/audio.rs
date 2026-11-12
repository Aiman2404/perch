//! Master volume of the default output device via Core Audio.

use serde::Serialize;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::core::{GUID, Result};

#[derive(Serialize, Default)]
pub struct Volume {
    pub level: f32,
    pub muted: bool,
}

fn endpoint() -> Result<IAudioEndpointVolume> {
    unsafe {
        // Tauri runs commands on a thread pool; make sure COM is up there.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
        dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
    }
}

fn read(ep: &IAudioEndpointVolume) -> Result<Volume> {
    unsafe {
        Ok(Volume {
            level: ep.GetMasterVolumeLevelScalar()?,
            muted: ep.GetMute()?.as_bool(),
        })
    }
}

pub fn get() -> Volume {
    endpoint().and_then(|ep| read(&ep)).unwrap_or_default()
}

pub fn set(level: f32) -> Volume {
    let level = level.clamp(0.0, 1.0);
    endpoint()
        .and_then(|ep| unsafe {
            ep.SetMasterVolumeLevelScalar(level, &GUID::zeroed())?;
            if level > 0.0 && ep.GetMute()?.as_bool() {
                ep.SetMute(false, &GUID::zeroed())?;
            }
            read(&ep)
        })
        .unwrap_or_default()
}

pub fn step(delta: f32) -> Volume {
    let cur = get().level;
    set(cur + delta)
}
