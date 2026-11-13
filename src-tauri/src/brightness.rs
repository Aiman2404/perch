//! Built-in display brightness through WMI (root\WMI WmiMonitorBrightness).
//! Setting brightness takes tens of milliseconds, so a worker applies only the
//! latest requested level and drops anything in between.

use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoSetProxyBlanket,
    EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::System::Wmi::{
    IWbemClassObject, IWbemLocator, IWbemServices, WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY,
    WBEM_INFINITE, WbemLocator,
};
use windows::core::{BSTR, Result, w};

enum Req {
    Get(Sender<Option<u8>>),
    Set(u8),
}

static TX: OnceLock<Mutex<Sender<Req>>> = OnceLock::new();

fn tx() -> Sender<Req> {
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Req>();
        thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let svc = connect().ok();
            while let Ok(first) = rx.recv() {
                // Drain the burst: apply only the last Set, then answer every Get.
                let mut last_set = None;
                let mut gets = Vec::new();
                for r in std::iter::once(first).chain(rx.try_iter()) {
                    match r {
                        Req::Set(v) => last_set = Some(v),
                        Req::Get(g) => gets.push(g),
                    }
                }
                if let (Some(v), Some(s)) = (last_set, &svc) {
                    let _ = apply(s, v);
                }
                if !gets.is_empty() {
                    let cur = svc.as_ref().and_then(|s| current(s).ok());
                    for g in gets {
                        let _ = g.send(cur);
                    }
                }
            }
        });
        Mutex::new(tx)
    })
    .lock()
    .unwrap()
    .clone()
}

/// Current brightness 0-100, or None when the display isn't controllable (e.g. desktop monitors).
pub fn get() -> Option<u8> {
    let (r, rx) = mpsc::channel();
    tx().send(Req::Get(r)).ok()?;
    rx.recv().ok().flatten()
}

pub fn set(level: u8) {
    let _ = tx().send(Req::Set(level.min(100)));
}

fn connect() -> Result<IWbemServices> {
    unsafe {
        let loc: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
        let svc = loc.ConnectServer(&BSTR::from("ROOT\\WMI"), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None)?;
        CoSetProxyBlanket(
            &svc,
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            None,
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )?;
        Ok(svc)
    }
}

fn first(svc: &IWbemServices, query: &str) -> Result<IWbemClassObject> {
    unsafe {
        let en = svc.ExecQuery(
            &BSTR::from("WQL"),
            &BSTR::from(query),
            WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
            None,
        )?;
        let mut objs = [None];
        let mut n = 0u32;
        en.Next(WBEM_INFINITE, &mut objs, &mut n).ok()?;
        objs[0].take().ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))
    }
}

fn current(svc: &IWbemServices) -> Result<u8> {
    unsafe {
        let obj = first(svc, "SELECT CurrentBrightness FROM WmiMonitorBrightness WHERE Active=TRUE")?;
        let mut v = VARIANT::default();
        obj.Get(w!("CurrentBrightness"), 0, &mut v, None, None)?;
        // CurrentBrightness is a uint8 (VT_UI1); read it straight out of the union.
        Ok(if v.Anonymous.Anonymous.vt.0 == 17 {
            v.Anonymous.Anonymous.Anonymous.bVal
        } else {
            i32::try_from(&v)? as u8
        })
    }
}

fn apply(svc: &IWbemServices, level: u8) -> Result<()> {
    unsafe {
        let inst = first(svc, "SELECT * FROM WmiMonitorBrightnessMethods WHERE Active=TRUE")?;
        let mut path = VARIANT::default();
        inst.Get(w!("__PATH"), 0, &mut path, None, None)?;
        let path = BSTR::try_from(&path)?;

        let mut class = None;
        svc.GetObject(&BSTR::from("WmiMonitorBrightnessMethods"), Default::default(), None, Some(&mut class), None)?;
        let class = class.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
        let mut sig = None;
        class.GetMethod(w!("WmiSetBrightness"), 0, &mut sig, std::ptr::null_mut())?;
        let sig = sig.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
        let args = sig.SpawnInstance(0)?;
        args.Put(w!("Timeout"), 0, &VARIANT::from(0i32), 0)?;
        args.Put(w!("Brightness"), 0, &VARIANT::from(level), 0)?;
        svc.ExecMethod(&path, &BSTR::from("WmiSetBrightness"), Default::default(), None, &args, None, None)?;
        Ok(())
    }
}
