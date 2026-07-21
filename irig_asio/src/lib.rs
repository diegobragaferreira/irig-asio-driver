//! `irig_asio` — Windows ASIO driver DLL for the IK Multimedia iRig USB.
//!
//! # DLL exports
//!
//! | Export                | Caller           | Purpose                                    |
//! |----------------------|------------------|--------------------------------------------|
//! | `DllGetClassObject`  | CoCreateInstance | Return an `IClassFactory` for our CLSID    |
//! | `DllCanUnloadNow`    | COM runtime      | Return S_OK if no objects are alive        |
//! | `DllRegisterServer`  | regsvr32         | Write ASIO + COM registry keys             |
//! | `DllUnregisterServer`| regsvr32 /u      | Remove registry keys                       |

#![allow(non_snake_case, clippy::missing_safety_doc)]
#![cfg(windows)]

pub mod asio_bindings;
pub mod asio_com;
pub mod asio_driver;
pub mod buffer_manager;
pub mod com_server;
pub mod constants;
pub mod ks_device;
pub mod ks_engine;
pub mod registration;
pub mod wasapi_backend;

use std::ffi::c_void;

use windows::core::{GUID, HRESULT};
use windows::Win32::Foundation::{BOOL, HMODULE, S_FALSE};

use crate::com_server::{lock_count, ClassFactory};
use crate::constants::{DRIVER_CLSID_STR, E_NOINTERFACE, S_OK};
use crate::registration::{get_dll_path, register_driver, unregister_driver};

// ---------------------------------------------------------------------------
// Module-level state
// ---------------------------------------------------------------------------

static mut MODULE_HANDLE: HMODULE = HMODULE(std::ptr::null_mut());

// ---------------------------------------------------------------------------
// DllMain
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "system" fn DllMain(
    module: HMODULE,
    reason: u32,
    _reserved: *mut c_void,
) -> BOOL {
    // reason constants: 1 = DLL_PROCESS_ATTACH, 0 = DLL_PROCESS_DETACH
    if reason == 1 {
        MODULE_HANDLE = module;
    }
    BOOL(1)
}

// ---------------------------------------------------------------------------
// DllGetClassObject
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    _riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if rclsid.is_null() || ppv.is_null() {
        return E_NOINTERFACE;
    }

    let our_clsid = parse_guid(DRIVER_CLSID_STR);
    if *rclsid != our_clsid {
        log::warn!("[COM] DllGetClassObject: unknown CLSID");
        return E_NOINTERFACE;
    }

    let factory = ClassFactory::new();
    *ppv = Box::into_raw(factory) as *mut c_void;
    log::info!("[COM] DllGetClassObject: returning ClassFactory");
    S_OK
}

// ---------------------------------------------------------------------------
// DllCanUnloadNow
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "system" fn DllCanUnloadNow() -> HRESULT {
    if lock_count() == 0 { S_OK } else { S_FALSE }
}

// ---------------------------------------------------------------------------
// DllRegisterServer / DllUnregisterServer
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "system" fn DllRegisterServer() -> HRESULT {
    let dll_path = get_dll_path(MODULE_HANDLE);
    match register_driver(&dll_path) {
        Ok(_)  => S_OK,
        Err(e) => {
            log::error!("[Registration] DllRegisterServer failed: {e}");
            e.code()
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn DllUnregisterServer() -> HRESULT {
    match unregister_driver() {
        Ok(_)  => S_OK,
        Err(e) => {
            log::error!("[Registration] DllUnregisterServer failed: {e}");
            e.code()
        }
    }
}

// ---------------------------------------------------------------------------
// GUID parsing helper
// ---------------------------------------------------------------------------

fn parse_guid(s: &str) -> GUID {
    let s = s.trim_matches(|c| c == '{' || c == '}');
    let parts: Vec<&str> = s.split('-').collect();
    assert_eq!(parts.len(), 5, "Invalid GUID format");

    let data1 = u32::from_str_radix(parts[0], 16).unwrap();
    let data2 = u16::from_str_radix(parts[1], 16).unwrap();
    let data3 = u16::from_str_radix(parts[2], 16).unwrap();

    let d4_str = format!("{}{}", parts[3], parts[4]);
    let mut data4 = [0u8; 8];
    for i in 0..8 {
        data4[i] = u8::from_str_radix(&d4_str[i * 2..i * 2 + 2], 16).unwrap();
    }

    GUID::from_values(data1, data2, data3, data4)
}
