//! IASIO COM vtable — bridges the C++ virtual dispatch ABI to our Rust driver.
//!
//! The ASIO host (DAW) calls `CoCreateInstance(CLSID, NULL, CLSCTX_INPROC,
//! IID_IASIO, &pDriver)` and expects back a pointer to an object whose first
//! field is a pointer to a vtable with this exact layout:
//!
//!  [0] QueryInterface   ← IUnknown
//!  [1] AddRef
//!  [2] Release
//!  [3] init             ← IASIO
//!  [4] getDriverName
//!  [5] getDriverVersion
//!  [6] getErrorMessage
//!  [7] start
//!  [8] stop
//!  [9] getChannels
//! [10] getLatencies
//! [11] getBufferSize
//! [12] canSampleRate
//! [13] getSampleRate
//! [14] setSampleRate
//! [15] getClockSources
//! [16] setClockSource
//! [17] getSamplePosition
//! [18] getChannelInfo
//! [19] createBuffers
//! [20] disposeBuffers
//! [21] controlPanel
//! [22] future
//! [23] outputReady

#![allow(non_snake_case, clippy::missing_safety_doc)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, Ordering};

use windows::core::{GUID, HRESULT};

use crate::asio_driver::{
    AsioBufferInfo, AsioCallbacks, AsioChannelInfo, IrigAsioDriver,
    ASIOBool, ASIOSampleRate, ASIO_FALSE,
};
use crate::constants::*;

// ---------------------------------------------------------------------------
// IID_IASIO — in ASIO each driver uses its own CLSID as both the CLSID and
// the interface IID. So IID_IASIO == DRIVER_CLSID_STR for this driver.
// ---------------------------------------------------------------------------

const IID_IASIO: GUID = GUID::from_values(
    0x8F3D4A2B, 0xE1C7, 0x4F89,
    [0xA0, 0xD3, 0x6B, 0x2E, 0x9C, 0x1F, 0x58, 0x47],
);

const IID_IUNKNOWN: GUID = GUID::from_values(
    0x00000000, 0x0000, 0x0000,
    [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
);

// ASIO future() selector constants
const KASIO_SELECTOR_SUPPORTED:   i32 = 1;
const KASIO_ENGINE_VERSION:        i32 = 2;
const KASIO_RESET_REQUEST:         i32 = 3;
const KASIO_SUPPORTS_TIME_INFO:    i32 = 7;
const KASIO_SUPPORTS_TIME_CODE:    i32 = 8;

// ---------------------------------------------------------------------------
// The IASIO vtable struct — mirrors the C++ vtable exactly.
// All entries use extern "system" which maps to:
//   - extern "C"       on x64  (no calling convention difference)
//   - extern "stdcall" on x86  (correct thiscall approximation)
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct IAsioVtbl {
    // IUnknown (3)
    pub QueryInterface:    unsafe extern "system" fn(*mut IrigAsioDriverCom, *const GUID, *mut *mut c_void) -> HRESULT,
    pub AddRef:            unsafe extern "system" fn(*mut IrigAsioDriverCom) -> u32,
    pub Release:           unsafe extern "system" fn(*mut IrigAsioDriverCom) -> u32,
    // IASIO (21)
    pub init:              unsafe extern "system" fn(*mut IrigAsioDriverCom, *mut c_void) -> ASIOBool,
    pub getDriverName:     unsafe extern "system" fn(*mut IrigAsioDriverCom, *mut u8),
    pub getDriverVersion:  unsafe extern "system" fn(*mut IrigAsioDriverCom) -> i32,
    pub getErrorMessage:   unsafe extern "system" fn(*mut IrigAsioDriverCom, *mut u8),
    pub start:             unsafe extern "system" fn(*mut IrigAsioDriverCom) -> i32,
    pub stop:              unsafe extern "system" fn(*mut IrigAsioDriverCom) -> i32,
    pub getChannels:       unsafe extern "system" fn(*mut IrigAsioDriverCom, *mut i32, *mut i32) -> i32,
    pub getLatencies:      unsafe extern "system" fn(*mut IrigAsioDriverCom, *mut i32, *mut i32) -> i32,
    pub getBufferSize:     unsafe extern "system" fn(*mut IrigAsioDriverCom, *mut i32, *mut i32, *mut i32, *mut i32) -> i32,
    pub canSampleRate:     unsafe extern "system" fn(*mut IrigAsioDriverCom, ASIOSampleRate) -> i32,
    pub getSampleRate:     unsafe extern "system" fn(*mut IrigAsioDriverCom, *mut ASIOSampleRate) -> i32,
    pub setSampleRate:     unsafe extern "system" fn(*mut IrigAsioDriverCom, ASIOSampleRate) -> i32,
    pub getClockSources:   unsafe extern "system" fn(*mut IrigAsioDriverCom, *mut c_void, *mut i32) -> i32,
    pub setClockSource:    unsafe extern "system" fn(*mut IrigAsioDriverCom, i32) -> i32,
    pub getSamplePosition: unsafe extern "system" fn(*mut IrigAsioDriverCom, *mut i64, *mut i64) -> i32,
    pub getChannelInfo:    unsafe extern "system" fn(*mut IrigAsioDriverCom, *mut AsioChannelInfo) -> i32,
    pub createBuffers:     unsafe extern "system" fn(*mut IrigAsioDriverCom, *mut AsioBufferInfo, i32, i32, *const AsioCallbacks) -> i32,
    pub disposeBuffers:    unsafe extern "system" fn(*mut IrigAsioDriverCom) -> i32,
    pub controlPanel:      unsafe extern "system" fn(*mut IrigAsioDriverCom) -> i32,
    pub future:            unsafe extern "system" fn(*mut IrigAsioDriverCom, i32, *mut c_void) -> i32,
    pub outputReady:       unsafe extern "system" fn(*mut IrigAsioDriverCom) -> i32,
}

// ---------------------------------------------------------------------------
// IrigAsioDriverCom — the COM object the DAW receives.
//
// Memory layout (required by C++ ABI):
//   offset 0: *const IAsioVtbl   ← vtable pointer (MUST be first)
//   offset 8: AtomicI32          ← COM reference count
//   offset N: IrigAsioDriver     ← our actual driver
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct IrigAsioDriverCom {
    vtable:    *const IAsioVtbl,
    ref_count: AtomicI32,
    driver:    IrigAsioDriver,
}

// SAFETY: IrigAsioDriverCom is only accessed through a single-apartment COM
// model. IrigAsioDriver uses Arc<Mutex<_>> internally for thread safety.
unsafe impl Send for IrigAsioDriverCom {}
unsafe impl Sync for IrigAsioDriverCom {}

// Static vtable — one instance shared by all driver objects.
static IASIO_VTBL: IAsioVtbl = IAsioVtbl {
    QueryInterface:    iasio_query_interface,
    AddRef:            iasio_add_ref,
    Release:           iasio_release,
    init:              iasio_init,
    getDriverName:     iasio_get_driver_name,
    getDriverVersion:  iasio_get_driver_version,
    getErrorMessage:   iasio_get_error_message,
    start:             iasio_start,
    stop:              iasio_stop,
    getChannels:       iasio_get_channels,
    getLatencies:      iasio_get_latencies,
    getBufferSize:     iasio_get_buffer_size,
    canSampleRate:     iasio_can_sample_rate,
    getSampleRate:     iasio_get_sample_rate,
    setSampleRate:     iasio_set_sample_rate,
    getClockSources:   iasio_get_clock_sources,
    setClockSource:    iasio_set_clock_source,
    getSamplePosition: iasio_get_sample_position,
    getChannelInfo:    iasio_get_channel_info,
    createBuffers:     iasio_create_buffers,
    disposeBuffers:    iasio_dispose_buffers,
    controlPanel:      iasio_control_panel,
    future:            iasio_future,
    outputReady:       iasio_output_ready,
};

impl IrigAsioDriverCom {
    pub fn new() -> Box<Self> {
        Box::new(Self {
            vtable:    &IASIO_VTBL,
            ref_count: AtomicI32::new(1),
            driver:    IrigAsioDriver::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// IUnknown implementation
// ---------------------------------------------------------------------------

pub(crate) unsafe extern "system" fn iasio_query_interface(
    this: *mut IrigAsioDriverCom,
    iid:  *const GUID,
    ppv:  *mut *mut c_void,
) -> HRESULT {
    if this.is_null() || iid.is_null() || ppv.is_null() {
        return E_NOINTERFACE;
    }
    let iid = &*iid;
    if *iid == IID_IUNKNOWN || *iid == IID_IASIO {
        *ppv = this as *mut c_void;
        iasio_add_ref(this);
        log::debug!("[COM] QueryInterface -> IASIO OK");
        S_OK
    } else {
        *ppv = std::ptr::null_mut();
        log::debug!("[COM] QueryInterface -> E_NOINTERFACE");
        E_NOINTERFACE
    }
}

unsafe extern "system" fn iasio_add_ref(this: *mut IrigAsioDriverCom) -> u32 {
    if this.is_null() { return 0; }
    ((*this).ref_count.fetch_add(1, Ordering::SeqCst) + 1) as u32
}

pub(crate) unsafe extern "system" fn iasio_release(this: *mut IrigAsioDriverCom) -> u32 {
    if this.is_null() { return 0; }
    let prev = (*this).ref_count.fetch_sub(1, Ordering::SeqCst);
    if prev == 1 {
        log::info!("[COM] IrigAsioDriverCom released — dropping");
        drop(Box::from_raw(this));
        return 0;
    }
    (prev - 1) as u32
}

// ---------------------------------------------------------------------------
// IASIO implementation — thin wrappers forwarding to IrigAsioDriver
// ---------------------------------------------------------------------------

unsafe extern "system" fn iasio_init(
    this: *mut IrigAsioDriverCom,
    sys_handle: *mut c_void,
) -> ASIOBool {
    log::info!("[IASIO] init()");
    if this.is_null() { return ASIO_FALSE; }
    (*this).driver.asio_init(sys_handle)
}

unsafe extern "system" fn iasio_get_driver_name(
    this: *mut IrigAsioDriverCom,
    name: *mut u8,
) {
    if this.is_null() || name.is_null() { return; }
    let mut buf = [0u8; 32];
    (*this).driver.asio_get_driver_name(&mut buf);
    std::ptr::copy_nonoverlapping(buf.as_ptr(), name, 32);
}

unsafe extern "system" fn iasio_get_driver_version(
    this: *mut IrigAsioDriverCom,
) -> i32 {
    if this.is_null() { return 0; }
    (*this).driver.asio_get_driver_version()
}

unsafe extern "system" fn iasio_get_error_message(
    this: *mut IrigAsioDriverCom,
    msg: *mut u8,
) {
    if this.is_null() || msg.is_null() { return; }
    let mut buf = [0u8; 128];
    (*this).driver.asio_get_error_message(&mut buf);
    std::ptr::copy_nonoverlapping(buf.as_ptr(), msg, 128);
}

unsafe extern "system" fn iasio_start(this: *mut IrigAsioDriverCom) -> i32 {
    log::info!("[IASIO] start()");
    if this.is_null() { return ASE_INVALID_PARAMETER; }
    (*this).driver.asio_start()
}

unsafe extern "system" fn iasio_stop(this: *mut IrigAsioDriverCom) -> i32 {
    log::info!("[IASIO] stop()");
    if this.is_null() { return ASE_INVALID_PARAMETER; }
    (*this).driver.asio_stop()
}

unsafe extern "system" fn iasio_get_channels(
    this: *mut IrigAsioDriverCom,
    num_in:  *mut i32,
    num_out: *mut i32,
) -> i32 {
    if this.is_null() || num_in.is_null() || num_out.is_null() {
        return ASE_INVALID_PARAMETER;
    }
    (*this).driver.asio_get_channels(&mut *num_in, &mut *num_out)
}

unsafe extern "system" fn iasio_get_latencies(
    this: *mut IrigAsioDriverCom,
    in_lat:  *mut i32,
    out_lat: *mut i32,
) -> i32 {
    if this.is_null() || in_lat.is_null() || out_lat.is_null() {
        return ASE_INVALID_PARAMETER;
    }
    (*this).driver.asio_get_latencies(&mut *in_lat, &mut *out_lat)
}

unsafe extern "system" fn iasio_get_buffer_size(
    this: *mut IrigAsioDriverCom,
    min_size:   *mut i32,
    max_size:   *mut i32,
    pref_size:  *mut i32,
    granularity:*mut i32,
) -> i32 {
    if this.is_null() || min_size.is_null() || max_size.is_null() || pref_size.is_null() || granularity.is_null() {
        return ASE_INVALID_PARAMETER;
    }
    (*this).driver.asio_get_buffer_size(
        &mut *min_size, &mut *max_size, &mut *pref_size, &mut *granularity,
    )
}

unsafe extern "system" fn iasio_can_sample_rate(
    this: *mut IrigAsioDriverCom,
    rate: ASIOSampleRate,
) -> i32 {
    if this.is_null() { return ASE_INVALID_PARAMETER; }
    (*this).driver.asio_can_sample_rate(rate)
}

unsafe extern "system" fn iasio_get_sample_rate(
    this: *mut IrigAsioDriverCom,
    rate: *mut ASIOSampleRate,
) -> i32 {
    if this.is_null() || rate.is_null() {
        return ASE_INVALID_PARAMETER;
    }
    (*this).driver.asio_get_sample_rate(&mut *rate)
}

unsafe extern "system" fn iasio_set_sample_rate(
    this: *mut IrigAsioDriverCom,
    rate: ASIOSampleRate,
) -> i32 {
    if this.is_null() { return ASE_INVALID_PARAMETER; }
    (*this).driver.asio_set_sample_rate(rate)
}

unsafe extern "system" fn iasio_get_clock_sources(
    this: *mut IrigAsioDriverCom,
    _clocks:     *mut c_void,  // ASIOClockSource* — we have 1 internal clock
    num_sources: *mut i32,
) -> i32 {
    if this.is_null() || num_sources.is_null() {
        return ASE_INVALID_PARAMETER;
    }
    *num_sources = 1;
    ASE_OK
}

unsafe extern "system" fn iasio_set_clock_source(
    this: *mut IrigAsioDriverCom,
    _reference: i32,
) -> i32 {
    if this.is_null() { return ASE_INVALID_PARAMETER; }
    ASE_OK // single internal clock
}

unsafe extern "system" fn iasio_get_sample_position(
    this: *mut IrigAsioDriverCom,
    s_pos:     *mut i64,
    time_stamp: *mut i64,
) -> i32 {
    if this.is_null() || s_pos.is_null() || time_stamp.is_null() {
        return ASE_INVALID_PARAMETER;
    }
    (*this).driver.asio_get_sample_position(&mut *s_pos, &mut *time_stamp)
}

unsafe extern "system" fn iasio_get_channel_info(
    this: *mut IrigAsioDriverCom,
    info: *mut AsioChannelInfo,
) -> i32 {
    if this.is_null() || info.is_null() {
        return ASE_INVALID_PARAMETER;
    }
    (*this).driver.asio_get_channel_info(&mut *info)
}

unsafe extern "system" fn iasio_create_buffers(
    this:      *mut IrigAsioDriverCom,
    infos:     *mut AsioBufferInfo,
    num_ch:    i32,
    buf_size:  i32,
    callbacks: *const AsioCallbacks,
) -> i32 {
    if this.is_null() || infos.is_null() || callbacks.is_null() {
        return ASE_INVALID_PARAMETER;
    }
    log::info!("[IASIO] createBuffers({} ch, {} frames)", num_ch, buf_size);
    (*this).driver.asio_create_buffers(infos, num_ch, buf_size, callbacks)
}

unsafe extern "system" fn iasio_dispose_buffers(this: *mut IrigAsioDriverCom) -> i32 {
    log::info!("[IASIO] disposeBuffers()");
    if this.is_null() { return ASE_INVALID_PARAMETER; }
    (*this).driver.asio_dispose_buffers()
}

unsafe extern "system" fn iasio_control_panel(this: *mut IrigAsioDriverCom) -> i32 {
    if this.is_null() { return ASE_INVALID_PARAMETER; }
    (*this).driver.asio_control_panel()
}

unsafe extern "system" fn iasio_future(
    this:     *mut IrigAsioDriverCom,
    selector: i32,
    _opt:     *mut c_void,
) -> i32 {
    if this.is_null() { return ASE_INVALID_PARAMETER; }
    // The host queries us about optional features.
    log::debug!("[IASIO] future(selector={})", selector);
    match selector {
        // kAsioEngineVersion: return ASIO 2.x support
        s if s == KASIO_ENGINE_VERSION => 2,
        // kAsioSupportsTimeInfo: we do NOT yet support ASIOTime structs
        s if s == KASIO_SUPPORTS_TIME_INFO => ASE_NOT_PRESENT,
        // kAsioSupportsTimeCode
        s if s == KASIO_SUPPORTS_TIME_CODE => ASE_NOT_PRESENT,
        // kAsioResetRequest: acknowledge but do nothing
        s if s == KASIO_RESET_REQUEST => ASE_OK,
        // kAsioSelectorSupported: answer per-selector
        s if s == KASIO_SELECTOR_SUPPORTED => ASE_NOT_PRESENT,
        _ => ASE_NOT_PRESENT,
    }
}

unsafe extern "system" fn iasio_output_ready(this: *mut IrigAsioDriverCom) -> i32 {
    if this.is_null() { return ASE_INVALID_PARAMETER; }
    (*this).driver.asio_output_ready()
}
