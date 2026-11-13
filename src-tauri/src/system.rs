//! Small system bits: battery, dark mode, Wi-Fi/Bluetooth radios, start with
//! Windows, and how much memory Perch itself is using.

use serde::Serialize;
use windows::Devices::Radios::{Radio, RadioKind, RadioState};
use windows::Win32::Foundation::{CloseHandle, LPARAM, WPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_DWORD, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegCloseKey,
    RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::Win32::System::Threading::{GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::WindowsAndMessaging::{HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE};
use windows::core::{HSTRING, PCWSTR, w};

// ---------- battery ----------

#[derive(Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Power {
    pub has_battery: bool,
    pub percent: u8,
    pub charging: bool,
}

pub fn power() -> Power {
    let mut s = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut s) }.is_err() {
        return Power { has_battery: false, percent: 100, charging: false };
    }
    Power {
        has_battery: s.BatteryFlag != 128 && s.BatteryFlag != 255,
        percent: if s.BatteryLifePercent == 255 { 100 } else { s.BatteryLifePercent },
        charging: s.ACLineStatus == 1,
    }
}

// ---------- dark mode ----------

const PERSONALIZE: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");

fn read_dword(key: PCWSTR, name: PCWSTR) -> Option<u32> {
    let mut v = 0u32;
    let mut size = 4u32;
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key, name, RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as *mut _), Some(&mut size))
            .ok()
            .ok()?;
    }
    Some(v)
}

fn open(key: PCWSTR) -> Option<HKEY> {
    let mut h = HKEY::default();
    unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key, None, KEY_SET_VALUE, &mut h).ok().ok()? };
    Some(h)
}

pub fn dark_mode() -> bool {
    read_dword(PERSONALIZE, w!("AppsUseLightTheme")).map(|v| v == 0).unwrap_or(false)
}

pub fn set_dark_mode(dark: bool) -> bool {
    let Some(h) = open(PERSONALIZE) else { return false };
    let v: u32 = if dark { 0 } else { 1 };
    let bytes = v.to_le_bytes();
    unsafe {
        let _ = RegSetValueExW(h, w!("AppsUseLightTheme"), None, REG_DWORD, Some(&bytes));
        let _ = RegSetValueExW(h, w!("SystemUsesLightTheme"), None, REG_DWORD, Some(&bytes));
        let _ = RegCloseKey(h);
        // Tell running apps and the shell the colour scheme changed.
        let topic = HSTRING::from("ImmersiveColorSet");
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(topic.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            150,
            None,
        );
    }
    dark_mode() == dark
}

// ---------- radios ----------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Radios {
    /// None = this PC has no such radio (or Windows won't let us see it).
    pub wifi: Option<bool>,
    pub bluetooth: Option<bool>,
}

fn find(kind: RadioKind) -> Option<Radio> {
    let _ = Radio::RequestAccessAsync().ok()?.join();
    let list = Radio::GetRadiosAsync().ok()?.join().ok()?;
    list.into_iter().find(|r| r.Kind().ok() == Some(kind))
}

pub fn radios() -> Radios {
    let state = |k| find(k).and_then(|r| r.State().ok()).map(|s| s == RadioState::On);
    Radios { wifi: state(RadioKind::WiFi), bluetooth: state(RadioKind::Bluetooth) }
}

pub fn set_radio(which: &str, on: bool) -> bool {
    let kind = if which == "wifi" { RadioKind::WiFi } else { RadioKind::Bluetooth };
    let Some(r) = find(kind) else { return false };
    let target = if on { RadioState::On } else { RadioState::Off };
    r.SetStateAsync(target).ok().and_then(|op| op.join().ok()).is_some() && r.State().ok() == Some(target)
}

// ---------- start with Windows ----------

const RUN: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

pub fn autostart() -> bool {
    let mut size = 0u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN, w!("Perch"), RRF_RT_REG_SZ, None, None, Some(&mut size)).is_ok() }
}

pub fn set_autostart(on: bool) -> bool {
    let Some(h) = open(RUN) else { return false };
    unsafe {
        if on {
            let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            let cmd: Vec<u16> = format!("\"{exe}\"").encode_utf16().chain([0]).collect();
            let bytes = std::slice::from_raw_parts(cmd.as_ptr() as *const u8, cmd.len() * 2);
            let _ = RegSetValueExW(h, w!("Perch"), None, REG_SZ, Some(bytes));
        } else {
            let _ = RegDeleteValueW(h, w!("Perch"));
        }
        let _ = RegCloseKey(h);
    }
    autostart() == on
}

// ---------- memory ----------

/// Private memory of Perch and every process it started (the WebView2 tree), in MB.
pub fn memory_mb() -> f64 {
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return 0.0 };
        let mut pairs = Vec::new();
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                pairs.push((e.th32ProcessID, e.th32ParentProcessID));
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);

        let mut tree = vec![GetCurrentProcessId()];
        loop {
            let before = tree.len();
            for (pid, ppid) in &pairs {
                if tree.contains(ppid) && !tree.contains(pid) {
                    tree.push(*pid);
                }
            }
            if tree.len() == before {
                break;
            }
        }

        let mut total = 0usize;
        for pid in tree {
            if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                let mut c = PROCESS_MEMORY_COUNTERS_EX::default();
                if GetProcessMemoryInfo(
                    h,
                    &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
                    std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
                )
                .is_ok()
                {
                    total += c.PrivateUsage;
                }
                let _ = CloseHandle(h);
            }
        }
        total as f64 / (1024.0 * 1024.0)
    }
}
