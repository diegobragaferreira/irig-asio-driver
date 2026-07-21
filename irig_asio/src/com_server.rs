//! COM class factory — `IClassFactory` implementation for `IrigAsioDriver`.
//!
//! On x64 Windows, `extern "system"` == `extern "C"` (no stdcall).
//! On x86 Windows, `extern "system"` == `extern "stdcall"`.
//! Using `extern "system"` is correct for both and avoids the deprecation warning.

#![allow(non_snake_case, clippy::missing_safety_doc)]

use std::sync::atomic::{AtomicI32, Ordering};

use windows::core::{GUID, HRESULT};
use windows::Win32::Foundation::BOOL;

use crate::constants::{
    CLASS_E_NOAGGREGATION, E_NOINTERFACE, S_OK,
};

// ---------------------------------------------------------------------------
// Server lock count (for DllCanUnloadNow)
// ---------------------------------------------------------------------------

static SERVER_LOCK_COUNT: AtomicI32 = AtomicI32::new(0);

pub fn increment_lock_count() { SERVER_LOCK_COUNT.fetch_add(1, Ordering::SeqCst); }
pub fn decrement_lock_count() { SERVER_LOCK_COUNT.fetch_sub(1, Ordering::SeqCst); }
pub fn lock_count() -> i32    { SERVER_LOCK_COUNT.load(Ordering::SeqCst) }

// ---------------------------------------------------------------------------
// IUnknown / IClassFactory GUIDs
// ---------------------------------------------------------------------------

/// IID_IUnknown  {00000000-0000-0000-C000-000000000046}
const IID_IUNKNOWN: GUID = GUID::from_values(
    0x00000000, 0x0000, 0x0000,
    [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
);

/// IID_IClassFactory  {00000001-0000-0000-C000-000000000046}
const IID_ICLASSFACTORY: GUID = GUID::from_values(
    0x00000001, 0x0000, 0x0000,
    [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
);

// ---------------------------------------------------------------------------
// ClassFactory
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct ClassFactory {
    vtable: *const IClassFactoryVtbl,
    ref_count: AtomicI32,
}

#[repr(C)]
struct IClassFactoryVtbl {
    QueryInterface: unsafe extern "system" fn(*mut ClassFactory, *const GUID, *mut *mut std::ffi::c_void) -> HRESULT,
    AddRef:         unsafe extern "system" fn(*mut ClassFactory) -> u32,
    Release:        unsafe extern "system" fn(*mut ClassFactory) -> u32,
    CreateInstance: unsafe extern "system" fn(*mut ClassFactory, *mut std::ffi::c_void, *const GUID, *mut *mut std::ffi::c_void) -> HRESULT,
    LockServer:     unsafe extern "system" fn(*mut ClassFactory, BOOL) -> HRESULT,
}

static CLASS_FACTORY_VTBL: IClassFactoryVtbl = IClassFactoryVtbl {
    QueryInterface: cf_query_interface,
    AddRef:         cf_add_ref,
    Release:        cf_release,
    CreateInstance: cf_create_instance,
    LockServer:     cf_lock_server,
};

impl ClassFactory {
    pub fn new() -> Box<Self> {
        Box::new(Self {
            vtable: &CLASS_FACTORY_VTBL,
            ref_count: AtomicI32::new(1),
        })
    }
}

unsafe extern "system" fn cf_query_interface(
    this: *mut ClassFactory,
    iid: *const GUID,
    ppv: *mut *mut std::ffi::c_void,
) -> HRESULT {
    if iid.is_null() || ppv.is_null() { return E_NOINTERFACE; }
    let iid_ref = &*iid;
    if *iid_ref == IID_IUNKNOWN || *iid_ref == IID_ICLASSFACTORY {
        *ppv = this as *mut std::ffi::c_void;
        cf_add_ref(this);
        S_OK
    } else {
        *ppv = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn cf_add_ref(this: *mut ClassFactory) -> u32 {
    (*this).ref_count.fetch_add(1, Ordering::SeqCst) as u32 + 1
}

unsafe extern "system" fn cf_release(this: *mut ClassFactory) -> u32 {
    let prev = (*this).ref_count.fetch_sub(1, Ordering::SeqCst);
    if prev == 1 {
        drop(Box::from_raw(this));
        return 0;
    }
    (prev - 1) as u32
}

unsafe extern "system" fn cf_create_instance(
    _this: *mut ClassFactory,
    outer: *mut std::ffi::c_void,
    _iid: *const GUID,
    ppv: *mut *mut std::ffi::c_void,
) -> HRESULT {
    if !outer.is_null() {
        return CLASS_E_NOAGGREGATION;
    }
    use crate::asio_com::IrigAsioDriverCom;
    let driver = IrigAsioDriverCom::new();
    *ppv = Box::into_raw(driver) as *mut std::ffi::c_void;
    log::info!("[COM] IrigAsioDriverCom instance created (with IASIO vtable)");
    S_OK
}

unsafe extern "system" fn cf_lock_server(
    _this: *mut ClassFactory,
    lock: BOOL,
) -> HRESULT {
    if lock.as_bool() {
        increment_lock_count();
    } else {
        decrement_lock_count();
    }
    S_OK
}
