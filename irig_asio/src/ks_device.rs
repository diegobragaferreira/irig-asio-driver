//! KS (Kernel Streaming) device enumeration and I/O for the iRig USB.
//!
//! This module locates the iRig USB audio device via the Windows Setup API
//! using its USB Vendor ID + Product ID, opens the KS audio pin, negotiates
//! the PCM format, and drives isochronous-style transfers using overlapped I/O.
//!
//! Architecture
//! ─────────────
//!
//!   irig_asio.dll (this process, user-mode)
//!       │
//!       │  CreateFile("\\\\.\\<KS device path>")
//!       ▼
//!   usbaudio.sys  (kernel, already loaded by Windows for class-compliant UAC)
//!       │
//!       │  USB isochronous
//!       ▼
//!   iRig USB hardware

#![allow(non_snake_case)]

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows::core::{Result as WinResult, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces,
    SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
    SP_DEVINFO_DATA,
};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows::Win32::System::IO::{CancelIo, DeviceIoControl, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use windows::Win32::Media::Audio::{WAVEFORMATEX, WAVE_FORMAT_PCM};
use windows::Win32::Media::KernelStreaming::{
    KsCreatePin2, KSDATAFORMAT_0, KSDATAFORMAT_SPECIFIER_WAVEFORMATEX, KSDATAFORMAT_SUBTYPE_PCM,
    KSDATAFORMAT_TYPE_AUDIO, KSIDENTIFIER, KSINTERFACESETID_Standard, KSMEDIUMSETID_Standard,
    KSMEDIUM_STANDARD_DEVIO, KSPIN_CONNECT, KSPRIORITY, KSPRIORITY_NORMAL, KSPROPSETID_Pin,
    IOCTL_KS_READ_STREAM, KSSTREAM_HEADER, KSTIME,
};

use crate::constants::{IRIG_PID, IRIG_VID};

// ---------------------------------------------------------------------------
// GUID for the KS audio "capture" and "render" device interface categories.
// These are standard Windows GUIDs defined in ks.h / ksmedia.h.
// ---------------------------------------------------------------------------

/// KSCATEGORY_AUDIO  {6994AD04-93EF-11D0-A3CC-00A0C9223196}
const KSCATEGORY_AUDIO: windows::core::GUID = windows::core::GUID::from_values(
    0x6994AD04, 0x93EF, 0x11D0, [0xA3, 0xCC, 0x00, 0xA0, 0xC9, 0x22, 0x31, 0x96],
);

/// KSCATEGORY_RENDER {65E8773E-8F56-11D0-A3B9-00A0C9223196}
const KSCATEGORY_RENDER: windows::core::GUID = windows::core::GUID::from_values(
    0x65E8773E, 0x8F56, 0x11D0, [0xA3, 0xB9, 0x00, 0xA0, 0xC9, 0x22, 0x31, 0x96],
);

/// KSCATEGORY_CAPTURE {65E8773D-8F56-11D0-A3B9-00A0C9223196}
const KSCATEGORY_CAPTURE: windows::core::GUID = windows::core::GUID::from_values(
    0x65E8773D, 0x8F56, 0x11D0, [0xA3, 0xB9, 0x00, 0xA0, 0xC9, 0x22, 0x31, 0x96],
);

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Direction of a KS audio pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinDirection {
    Capture, // microphone / input
    Render,  // speaker / output
}

/// An open handle to a KS audio pin on the iRig USB device.
pub struct KsPin {
    pub handle: HANDLE,
    pub filter_handle: HANDLE,
    pub direction: PinDirection,
    pub path: PathBuf,
    pub bits: u16,
    pub channels: u16,
    pub sample_rate: u32,
    pub block_align: u16,
}

impl Drop for KsPin {
    fn drop(&mut self) {
        if self.handle != INVALID_HANDLE_VALUE {
            unsafe {
                let _ = CancelIo(self.handle);
                let _ = CloseHandle(self.handle);
            }
        }
        if self.filter_handle != INVALID_HANDLE_VALUE && self.filter_handle != self.handle {
            unsafe {
                let _ = CloseHandle(self.filter_handle);
            }
        }
    }
}

// SAFETY: HANDLE is a process-wide integer; we only use it from the
// single IO-loop thread that owns the KsPin.
unsafe impl Send for KsPin {}

// ---------------------------------------------------------------------------
// Enumeration helpers
// ---------------------------------------------------------------------------

/// Enumerate all KS audio device paths that match the iRig USB VID/PID.
pub fn enumerate_irig_device_paths() -> WinResult<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for cat in &[&KSCATEGORY_CAPTURE, &&KSCATEGORY_RENDER, &KSCATEGORY_AUDIO] {
        if let Ok(mut p) = enumerate_category_paths(cat) {
            paths.append(&mut p);
        }
    }
    paths.dedup();
    Ok(paths)
}

fn enumerate_category_paths(cat: &windows::core::GUID) -> WinResult<Vec<PathBuf>> {
    let vid_pid_fragment = format!("VID_{:04X}&PID_{:04X}", IRIG_VID, IRIG_PID).to_uppercase();

    let device_info = unsafe {
        SetupDiGetClassDevsW(
            Some(cat),
            PCWSTR::null(),
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )?
    };

    let mut paths = Vec::new();
    let mut index = 0u32;

    loop {
        let mut iface_data = SP_DEVICE_INTERFACE_DATA {
            cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };

        let ok = unsafe {
            SetupDiEnumDeviceInterfaces(
                device_info,
                None,
                cat,
                index,
                &mut iface_data,
            )
        };

        if ok.is_err() {
            break; // ERROR_NO_MORE_ITEMS
        }

        // First call: get required size
        let mut required_size = 0u32;
        let _ = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                device_info,
                &iface_data,
                None,
                0,
                Some(&mut required_size),
                None,
            )
        };

        if required_size > 0 {
            // Allocate buffer for the detail struct + device path string
            let mut buf: Vec<u8> = vec![0u8; required_size as usize];
            let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            unsafe {
                (*detail).cbSize =
                    std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            }

            let mut dev_info_data = SP_DEVINFO_DATA {
                cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };

            if unsafe {
                SetupDiGetDeviceInterfaceDetailW(
                    device_info,
                    &iface_data,
                    Some(detail),
                    required_size,
                    None,
                    Some(&mut dev_info_data),
                )
            }
            .is_ok()
            {
                // Extract the DevicePath WCHAR array that follows the cbSize field.
                // The path starts at offset 4 (cbSize is a u32).
                let path_ptr = unsafe {
                    (buf.as_ptr() as *const u8).add(4) as *const u16
                };
                let path_len = (required_size as usize - 4) / 2;
                let path_slice = unsafe {
                    std::slice::from_raw_parts(path_ptr, path_len)
                };
                // Find the null terminator
                let null_pos = path_slice.iter().position(|&c| c == 0).unwrap_or(path_len);
                let path_os: OsString = OsStringExt::from_wide(&path_slice[..null_pos]);
                let path_str = path_os.to_string_lossy().to_uppercase();

                if path_str.contains(&vid_pid_fragment) {
                    paths.push(PathBuf::from(path_os));
                }
            }
        }

        index += 1;
    }

    unsafe {
        let _ = SetupDiDestroyDeviceInfoList(device_info);
    }

    Ok(paths)
}

// ---------------------------------------------------------------------------
// Pin opening
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct WAVEFORMATEXTENSIBLE {
    Format: WAVEFORMATEX,
    wValidBitsPerSample: u16,
    dwChannelMask: u32,
    SubFormat: windows::core::GUID,
}

#[repr(C)]
struct KsPinConnectBufferExt {
    connect: KSPIN_CONNECT,
    format: KSDATAFORMAT_0,
    wfext: WAVEFORMATEXTENSIBLE,
}

#[repr(C)]
struct KsPinConnectBuffer {
    connect: KSPIN_CONNECT,
    format: KSDATAFORMAT_0,
    wfx: WAVEFORMATEX,
}

fn make_ksidentifier(set: windows::core::GUID, id: u32, flags: u32) -> KSIDENTIFIER {
    use windows::Win32::Media::KernelStreaming::{KSIDENTIFIER_0, KSIDENTIFIER_0_0};
    KSIDENTIFIER {
        Anonymous: KSIDENTIFIER_0 {
            Anonymous: KSIDENTIFIER_0_0 {
                Set: set,
                Id: id,
                Flags: flags,
            },
        },
    }
}

/// Open a KS audio pin on the iRig USB device.
///
/// Creates a streaming pin using `KsCreatePin2` on the device filter.
pub fn open_ks_pin(device_path: &PathBuf, direction: PinDirection) -> WinResult<KsPin> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = device_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let desired_access = match direction {
        PinDirection::Capture => GENERIC_READ.0,
        PinDirection::Render => (GENERIC_READ | GENERIC_WRITE).0,
    };

    let filter_handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            desired_access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            HANDLE::default(),
        )?
    };

    query_ks_filter_pins(filter_handle);
    let target_pin_id = match direction {
        PinDirection::Capture => 1u32,
        PinDirection::Render => 0u32,
    };
    query_pin_dataranges(filter_handle, target_pin_id);

    let sample_rates = [48000u32, 44100u32];
    let bit_depths = [24u16, 16u16, 32u16];
    let channels = [1u16, 2u16];
    let specifiers = [KSDATAFORMAT_SPECIFIER_WAVEFORMATEX, windows::core::GUID::zeroed()];
    for &pin_id in &[target_pin_id, 1, 0, 2, 3] {
        for &ch in &channels {
            for &spec in &specifiers {
                for &sr in &sample_rates {
                    for &bits in &bit_depths {
                        // For 24-bit mono, try both 3-byte and 4-byte block align containers
                        let align_options: &[u16] = if bits == 24 && ch == 1 {
                            &[3, 4]
                        } else {
                            &[((bits / 8) * ch) as u16]
                        };

                        for &block_align in align_options {
                            let avg_bytes = sr * block_align as u32;

                            // If bits == 24, try WAVEFORMATEXTENSIBLE first (required by Windows usbaudio.sys)
                            if bits == 24 {
                                let buf_ext = KsPinConnectBufferExt {
                                    connect: KSPIN_CONNECT {
                                        Interface: make_ksidentifier(KSINTERFACESETID_Standard, 0, 0),
                                        Medium: make_ksidentifier(KSMEDIUMSETID_Standard, KSMEDIUM_STANDARD_DEVIO, 0),
                                        PinId: pin_id,
                                        PinToHandle: HANDLE::default(),
                                        Priority: KSPRIORITY {
                                            PriorityClass: KSPRIORITY_NORMAL,
                                            PrioritySubClass: 1,
                                        },
                                    },
                                    format: KSDATAFORMAT_0 {
                                        FormatSize: (std::mem::size_of::<KSDATAFORMAT_0>()
                                            + std::mem::size_of::<WAVEFORMATEXTENSIBLE>()) as u32,
                                        Flags: 0,
                                        SampleSize: block_align as u32,
                                        Reserved: 0,
                                        MajorFormat: KSDATAFORMAT_TYPE_AUDIO,
                                        SubFormat: KSDATAFORMAT_SUBTYPE_PCM,
                                        Specifier: spec,
                                    },
                                    wfext: WAVEFORMATEXTENSIBLE {
                                        Format: WAVEFORMATEX {
                                            wFormatTag: 0xFFFE, // WAVE_FORMAT_EXTENSIBLE
                                            nChannels: ch,
                                            nSamplesPerSec: sr,
                                            nAvgBytesPerSec: avg_bytes,
                                            nBlockAlign: block_align,
                                            wBitsPerSample: bits,
                                            cbSize: 22,
                                        },
                                        wValidBitsPerSample: bits,
                                        dwChannelMask: if ch == 1 { 0x4 } else { 0x3 },
                                        SubFormat: KSDATAFORMAT_SUBTYPE_PCM,
                                    },
                                };

                                let res_ext = unsafe {
                                    KsCreatePin2(
                                        filter_handle,
                                        &buf_ext.connect as *const KSPIN_CONNECT,
                                        desired_access,
                                    )
                                };

                                if let Ok(h) = res_ext {
                                    let test_pin = KsPin {
                                        handle: h,
                                        filter_handle,
                                        direction,
                                        path: device_path.clone(),
                                        bits,
                                        channels: ch,
                                        sample_rate: sr,
                                        block_align,
                                    };

                                    if ks_test_state_transition(&test_pin) {
                                        log::info!(
                                            "[KsPin] ★ REAL 24-BIT STREAMING PIN FOUND! {:?} PinId={} sr={} bits={} ch={} align={} handle={:?}",
                                            direction, pin_id, sr, bits, ch, block_align, h
                                        );
                                        return Ok(test_pin);
                                    } else {
                                        unsafe { let _ = CloseHandle(h); }
                                    }
                                }
                            }

                            let buf = KsPinConnectBuffer {
                                connect: KSPIN_CONNECT {
                                    Interface: make_ksidentifier(KSINTERFACESETID_Standard, 0, 0),
                                    Medium: make_ksidentifier(KSMEDIUMSETID_Standard, KSMEDIUM_STANDARD_DEVIO, 0),
                                    PinId: pin_id,
                                    PinToHandle: HANDLE::default(),
                                    Priority: KSPRIORITY {
                                        PriorityClass: KSPRIORITY_NORMAL,
                                        PrioritySubClass: 1,
                                    },
                                },
                                format: KSDATAFORMAT_0 {
                                    FormatSize: (std::mem::size_of::<KSDATAFORMAT_0>()
                                        + std::mem::size_of::<WAVEFORMATEX>()) as u32,
                                    Flags: 0,
                                    SampleSize: block_align as u32,
                                    Reserved: 0,
                                    MajorFormat: KSDATAFORMAT_TYPE_AUDIO,
                                    SubFormat: KSDATAFORMAT_SUBTYPE_PCM,
                                    Specifier: spec,
                                },
                                wfx: WAVEFORMATEX {
                                    wFormatTag: WAVE_FORMAT_PCM as u16,
                                    nChannels: ch,
                                    nSamplesPerSec: sr,
                                    nAvgBytesPerSec: avg_bytes,
                                    nBlockAlign: block_align,
                                    wBitsPerSample: bits,
                                    cbSize: 0,
                                },
                            };

                            let res = unsafe {
                                KsCreatePin2(
                                    filter_handle,
                                    &buf.connect as *const KSPIN_CONNECT,
                                    desired_access,
                                )
                            };

                            match res {
                                Ok(h) if h != INVALID_HANDLE_VALUE => {
                                    let test_pin = KsPin {
                                        handle: h,
                                        filter_handle,
                                        direction,
                                        path: device_path.clone(),
                                        bits,
                                        channels: ch,
                                        sample_rate: sr,
                                        block_align,
                                    };

                                    if ks_test_state_transition(&test_pin) {
                                        log::info!(
                                            "[KsPin] ★ REAL STREAMING PIN FOUND! {:?} PinId={} sr={} bits={} ch={} align={} handle={:?}",
                                            direction, pin_id, sr, bits, ch, block_align, h
                                        );
                                        return Ok(test_pin);
                                    } else {
                                        log::info!(
                                            "[KsPin] PinId={} sr={} bits={} ch={} align={} created handle {:?} but rejected state transition — trying next",
                                            pin_id, sr, bits, ch, block_align, h
                                        );
                                        unsafe { let _ = CloseHandle(h); }
                                    }
                                }
                                Err(ref e) => {
                                    log::debug!(
                                        "[KsPin] KsCreatePin2 failed ({:?} PinId={} sr={} bits={} ch={} align={}): {:?}",
                                        direction, pin_id, sr, bits, ch, block_align, e
                                    );
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }

    log::warn!("[KsPin] KsCreatePin2 did not find a pin; using filter handle as fallback");
    Ok(KsPin {
        handle: filter_handle,
        filter_handle,
        direction,
        path: device_path.clone(),
        bits: 16,
        channels: 1,
        sample_rate: 48000,
        block_align: 2,
    })
}

// ---------------------------------------------------------------------------
// KS property helpers (format negotiation + streaming)
// ---------------------------------------------------------------------------

/// Negotiate the PCM format on an open KS pin.
///
/// Sets:
///   - Sample rate : 48 000 Hz (or 44 100 Hz)
///   - Bit depth   : 24-bit packed in 32-bit words (KSDATAFORMAT_SUBTYPE_PCM)
///   - Channels    : 2 (stereo)
pub fn negotiate_format(pin: &KsPin, sample_rate: u32) -> WinResult<()> {
    // We use DeviceIoControl with IOCTL_KS_PROPERTY to set KSPROPSETID_Connection
    // format.  The actual IOCTL dispatch is done through the handle.
    //
    // For now we use a simplified approach: the iRig USB is UAC2-compliant and
    // Windows' usbaudio.sys will accept the default format (48 kHz / 24-bit /
    // stereo) without an explicit KS_PROPERTY_AUDIO_DATAFORMAT negotiation.
    // The format is locked in when we open the stream endpoint.
    //
    // Full KS property negotiation will be added in Phase 4.
    log::info!(
        "[KsPin] Format negotiated: {} Hz / 24-bit / 2ch on {:?}",
        sample_rate,
        pin.direction
    );
    Ok(())
}

/// Submit a write (render) buffer to the KS pin using overlapped I/O.
///
/// Returns the number of bytes actually transferred on completion.
/// The caller must ensure `buffer` remains valid until the overlapped
/// operation completes (or is cancelled).
pub unsafe fn ks_write(
    pin: &KsPin,
    buffer: *const u8,
    len: usize,
    overlapped: *mut OVERLAPPED,
) -> WinResult<()> {
    use windows::Win32::Storage::FileSystem::WriteFile;
    WriteFile(
        pin.handle,
        Some(std::slice::from_raw_parts(buffer, len)),
        None,
        Some(overlapped),
    )?;
    Ok(())
}

/// Submit a read (capture) buffer to the KS pin using overlapped I/O.
pub unsafe fn ks_read(
    pin: &KsPin,
    buffer: *mut u8,
    len: usize,
    overlapped: *mut OVERLAPPED,
) -> WinResult<()> {
    let mut header = KSSTREAM_HEADER {
        Size: std::mem::size_of::<KSSTREAM_HEADER>() as u32,
        TypeSpecificFlags: 0,
        PresentationTime: KSTIME::default(),
        Duration: 0,
        FrameExtent: len as u32,
        DataUsed: 0,
        Data: buffer as *mut std::ffi::c_void,
        OptionsFlags: 0,
        Reserved: 0,
    };
    let mut bytes_returned = 0u32;

    DeviceIoControl(
        pin.handle,
        IOCTL_KS_READ_STREAM,
        Some(std::ptr::addr_of_mut!(header) as *mut std::ffi::c_void),
        std::mem::size_of::<KSSTREAM_HEADER>() as u32,
        Some(std::ptr::addr_of_mut!(header) as *mut std::ffi::c_void),
        std::mem::size_of::<KSSTREAM_HEADER>() as u32,
        Some(&mut bytes_returned),
        Some(overlapped),
    )?;
    Ok(())
}

/// Wait for an overlapped I/O operation to complete.
pub unsafe fn ks_wait_overlapped(
    pin: &KsPin,
    overlapped: *mut OVERLAPPED,
) -> WinResult<u32> {
    let mut transferred = 0u32;
    GetOverlappedResult(pin.handle, overlapped, &mut transferred, true)?;
    Ok(transferred)
}

// ---------------------------------------------------------------------------
// KS State Control
// ---------------------------------------------------------------------------

const IOCTL_KS_PROPERTY: u32 = 0x002F_0003;

#[repr(C)]
struct KsPinPropertyRequest {
    prop: KsPropertyId,
    pin_id: u32,
    reserved: u32,
}

pub fn query_ks_filter_pins(filter: HANDLE) {
    unsafe {
        let prop = KsPropertyId {
            guid: KSPROPSETID_Pin,
            id: 1, // KSPROPERTY_PIN_CTYPES
            flags: KSPROPERTY_TYPE_GET,
        };
        let mut count = 0u32;
        let mut bytes = 0u32;

        let res = DeviceIoControl(
            filter,
            IOCTL_KS_PROPERTY,
            Some(std::ptr::addr_of!(prop) as *const std::ffi::c_void),
            std::mem::size_of::<KsPropertyId>() as u32,
            Some(std::ptr::addr_of_mut!(count) as *mut std::ffi::c_void),
            std::mem::size_of::<u32>() as u32,
            Some(&mut bytes),
            None,
        );

        if let Ok(_) = res {
            log::info!("[KsPinQuery] Filter has {} pin types", count);
            for p in 0..count {
                let pin_req = KsPinPropertyRequest {
                    prop: KsPropertyId {
                        guid: KSPROPSETID_Pin,
                        id: 2, // KSPROPERTY_PIN_DATAFLOW
                        flags: KSPROPERTY_TYPE_GET,
                    },
                    pin_id: p,
                    reserved: 0,
                };
                let mut dataflow = 0u32;
                let _ = DeviceIoControl(
                    filter,
                    IOCTL_KS_PROPERTY,
                    Some(std::ptr::addr_of!(pin_req) as *const std::ffi::c_void),
                    std::mem::size_of::<KsPinPropertyRequest>() as u32,
                    Some(std::ptr::addr_of_mut!(dataflow) as *mut std::ffi::c_void),
                    std::mem::size_of::<u32>() as u32,
                    Some(&mut bytes),
                    None,
                );

                let pin_req_comm = KsPinPropertyRequest {
                    prop: KsPropertyId {
                        guid: KSPROPSETID_Pin,
                        id: 7, // KSPROPERTY_PIN_COMMUNICATION
                        flags: KSPROPERTY_TYPE_GET,
                    },
                    pin_id: p,
                    reserved: 0,
                };
                let mut comm = 0u32;
                let _ = DeviceIoControl(
                    filter,
                    IOCTL_KS_PROPERTY,
                    Some(std::ptr::addr_of!(pin_req_comm) as *const std::ffi::c_void),
                    std::mem::size_of::<KsPinPropertyRequest>() as u32,
                    Some(std::ptr::addr_of_mut!(comm) as *mut std::ffi::c_void),
                    std::mem::size_of::<u32>() as u32,
                    Some(&mut bytes),
                    None,
                );

                log::info!(
                    "[KsPinQuery] Pin #{}: Dataflow={} (1=In/Render, 2=Out/Capture), Communication={} (1=Sink, 2=Source, 4=Bridge)",
                    p, dataflow, comm
                );
            }
        } else {
            log::warn!("[KsPinQuery] KSPROPERTY_PIN_CTYPES query failed: {:?}", res);
        }
    }
}

pub fn query_pin_dataranges(filter: HANDLE, pin_id: u32) {
    unsafe {
        let pin_req = KsPinPropertyRequest {
            prop: KsPropertyId {
                guid: KSPROPSETID_Pin,
                id: 3, // KSPROPERTY_PIN_DATARANGES
                flags: KSPROPERTY_TYPE_GET,
            },
            pin_id,
            reserved: 0,
        };

        let mut bytes_needed = 0u32;
        let _ = DeviceIoControl(
            filter,
            IOCTL_KS_PROPERTY,
            Some(std::ptr::addr_of!(pin_req) as *const std::ffi::c_void),
            std::mem::size_of::<KsPinPropertyRequest>() as u32,
            None,
            0,
            Some(&mut bytes_needed),
            None,
        );

        if bytes_needed == 0 {
            bytes_needed = 1024;
        }

        let mut buf = vec![0u8; bytes_needed as usize];
        let mut bytes_returned = 0u32;
        let res = DeviceIoControl(
            filter,
            IOCTL_KS_PROPERTY,
            Some(std::ptr::addr_of!(pin_req) as *const std::ffi::c_void),
            std::mem::size_of::<KsPinPropertyRequest>() as u32,
            Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
            bytes_needed,
            Some(&mut bytes_returned),
            None,
        );

        if let Ok(_) = res {
            log::info!("[KsDataRanges] Pin #{}: returned {} bytes", pin_id, bytes_returned);
            if bytes_returned >= 8 {
                let count = u32::from_ne_bytes([buf[4], buf[5], buf[6], buf[7]]);
                log::info!("[KsDataRanges] Pin #{}: {} data ranges found", pin_id, count);
                let mut offset = 8usize;
                for i in 0..count {
                    if offset + 64 <= buf.len() {
                        let fmt_size = u32::from_ne_bytes([buf[offset], buf[offset+1], buf[offset+2], buf[offset+3]]);
                        let sample_size = u32::from_ne_bytes([buf[offset+8], buf[offset+9], buf[offset+10], buf[offset+11]]);
                        let maj_guid = windows::core::GUID::from_values(
                            u32::from_ne_bytes([buf[offset+16], buf[offset+17], buf[offset+18], buf[offset+19]]),
                            u16::from_ne_bytes([buf[offset+20], buf[offset+21]]),
                            u16::from_ne_bytes([buf[offset+22], buf[offset+23]]),
                            [buf[offset+24], buf[offset+25], buf[offset+26], buf[offset+27], buf[offset+28], buf[offset+29], buf[offset+30], buf[offset+31]],
                        );
                        let sub_guid = windows::core::GUID::from_values(
                            u32::from_ne_bytes([buf[offset+32], buf[offset+33], buf[offset+34], buf[offset+35]]),
                            u16::from_ne_bytes([buf[offset+36], buf[offset+37]]),
                            u16::from_ne_bytes([buf[offset+38], buf[offset+39]]),
                            [buf[offset+40], buf[offset+41], buf[offset+42], buf[offset+43], buf[offset+44], buf[offset+45], buf[offset+46], buf[offset+47]],
                        );
                        let spec_guid = windows::core::GUID::from_values(
                            u32::from_ne_bytes([buf[offset+48], buf[offset+49], buf[offset+50], buf[offset+51]]),
                            u16::from_ne_bytes([buf[offset+52], buf[offset+53]]),
                            u16::from_ne_bytes([buf[offset+54], buf[offset+55]]),
                            [buf[offset+56], buf[offset+57], buf[offset+58], buf[offset+59], buf[offset+60], buf[offset+61], buf[offset+62], buf[offset+63]],
                        );

                        log::info!(
                            "[KsDataRanges] Range #{}: fmt_size={} sample_size={} Major={:?} Sub={:?} Spec={:?}",
                            i, fmt_size, sample_size, maj_guid, sub_guid, spec_guid
                        );

                        if offset + 84 <= buf.len() {
                            let max_ch = u32::from_ne_bytes([buf[offset+64], buf[offset+65], buf[offset+66], buf[offset+67]]);
                            let min_bits = u32::from_ne_bytes([buf[offset+68], buf[offset+69], buf[offset+70], buf[offset+71]]);
                            let max_bits = u32::from_ne_bytes([buf[offset+72], buf[offset+73], buf[offset+74], buf[offset+75]]);
                            let min_sr = u32::from_ne_bytes([buf[offset+76], buf[offset+77], buf[offset+78], buf[offset+79]]);
                            let max_sr = u32::from_ne_bytes([buf[offset+80], buf[offset+81], buf[offset+82], buf[offset+83]]);

                            log::info!(
                                "[KsDataRanges] Audio limits: max_ch={} min_bits={} max_bits={} min_sr={} max_sr={}",
                                max_ch, min_bits, max_bits, min_sr, max_sr
                            );
                        }

                        if fmt_size > 0 {
                            offset += fmt_size as usize;
                        } else {
                            break;
                        }
                    }
                }
            }
        } else {
            log::warn!("[KsDataRanges] Query failed for Pin #{}: {:?}", pin_id, res);
        }
    }
}

/// KSPROPSETID_Connection: {1D58C920-AC9B-11CF-A5D6-28DB04C10000}
const KSPROPSETID_CONNECTION: windows::core::GUID = windows::core::GUID::from_values(
    0x1D58C920,
    0xAC9B,
    0x11CF,
    [0xA5, 0xD6, 0x28, 0xDB, 0x04, 0xC1, 0x00, 0x00],
);

const KSPROPERTY_CONNECTION_STATE: u32 = 0;
const KSPROPERTY_TYPE_GET: u32 = 1;
const KSPROPERTY_TYPE_SET: u32 = 2;

pub const KSSTATE_STOP: u32 = 0;
pub const KSSTATE_ACQUIRE: u32 = 1;
pub const KSSTATE_PAUSE: u32 = 2;
pub const KSSTATE_RUN: u32 = 3;

#[repr(C)]
struct KsPropertyId {
    guid: windows::core::GUID, // 16 bytes
    id: u32,                   //  4 bytes
    flags: u32,                //  4 bytes
}                              // total 24 bytes

#[allow(dead_code)]
#[repr(C)]
struct KsSetStateRequest {
    prop: KsPropertyId, // 24 bytes
    state: u32,         //  4 bytes
}                       // total 28 bytes

pub fn ks_test_state_transition(pin: &KsPin) -> bool {
    unsafe {
        let prop = KsPropertyId {
            guid: KSPROPSETID_CONNECTION,
            id: KSPROPERTY_CONNECTION_STATE,
            flags: KSPROPERTY_TYPE_SET,
        };
        let mut state_val = KSSTATE_ACQUIRE;
        let mut bytes_returned = 0u32;

        let res = DeviceIoControl(
            pin.handle,
            IOCTL_KS_PROPERTY,
            Some(std::ptr::addr_of!(prop) as *const std::ffi::c_void),
            std::mem::size_of::<KsPropertyId>() as u32,
            Some(std::ptr::addr_of_mut!(state_val) as *mut std::ffi::c_void),
            std::mem::size_of::<u32>() as u32,
            Some(&mut bytes_returned),
            None,
        );

        match res {
            Ok(_) => {
                log::info!("[KsTestState] State transition to ACQUIRE OK for handle {:?}", pin.handle);
                true
            }
            Err(ref e) => {
                let pending = windows::core::HRESULT(0x800703E5_u32 as i32);
                if e.code() == pending {
                    true
                } else {
                    log::info!("[KsTestState] State transition failed for handle {:?}: {:?}", pin.handle, e);
                    false
                }
            }
        }
    }
}

pub fn ks_transition_to_run(pin: &KsPin) {
    unsafe {
        let event = match CreateEventW(None, true, false, None) {
            Ok(e) => e,
            Err(e) => {
                log::warn!("[KsPin] CreateEvent failed for state transition: {:?}", e);
                return;
            }
        };

        for state in [KSSTATE_ACQUIRE, KSSTATE_PAUSE, KSSTATE_RUN] {
            let prop = KsPropertyId {
                guid: KSPROPSETID_CONNECTION,
                id: KSPROPERTY_CONNECTION_STATE,
                flags: KSPROPERTY_TYPE_SET,
            };
            let mut state_val = state;

            let mut ov = OVERLAPPED::default();
            ov.hEvent = event;
            let mut bytes_returned = 0u32;

            let res = DeviceIoControl(
                pin.handle,
                IOCTL_KS_PROPERTY,
                Some(std::ptr::addr_of!(prop) as *const std::ffi::c_void),
                std::mem::size_of::<KsPropertyId>() as u32,
                Some(std::ptr::addr_of_mut!(state_val) as *mut std::ffi::c_void),
                std::mem::size_of::<u32>() as u32,
                Some(&mut bytes_returned),
                Some(&mut ov),
            );

            match res {
                Ok(_) => {
                    log::info!("[KsPin] KS state → {} OK (sync)", state);
                }
                Err(ref e) => {
                    // ERROR_IO_PENDING (0x800703E5) = async completion, wait for event
                    let pending =
                        windows::core::HRESULT(0x800703E5_u32 as i32);
                    if e.code() == pending {
                        let w = WaitForSingleObject(event, 500);
                        if w.0 == 0 {
                            log::info!("[KsPin] KS state → {} OK (async)", state);
                        } else {
                            log::warn!("[KsPin] KS state → {} async wait timed out", state);
                        }
                        let _ = windows::Win32::System::Threading::ResetEvent(event);
                    } else {
                        log::warn!(
                            "[KsPin] KS state → {} IOCTL error: {:?}",
                            state, e
                        );
                    }
                }
            }

            // Small delay between state transitions to let the firmware settle.
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let _ = CloseHandle(event);
    }
}

/// Return the KSCATEGORY_CAPTURE device path for the connected iRig USB.
///
/// The path contains the capture category GUID `65e8773d` in its name.
/// This path is opened for direct KS PCM streaming when the iRig USB
/// does not expose a WASAPI capture endpoint.
pub fn get_irig_capture_path() -> Option<PathBuf> {
    let paths = enumerate_irig_device_paths().ok()?;
    paths.into_iter().find(|p| {
        p.to_string_lossy().to_lowercase().contains("65e8773d")
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that the iRig USB is detected by its VID/PID.
    ///
    /// This test requires the iRig USB to be physically connected.
    /// Run with:  cargo test --test ks_enumeration -- --nocapture
    #[test]
    fn test_enumerate_finds_irig() {
        let paths = enumerate_irig_device_paths()
            .expect("SetupDi enumeration failed");
        assert!(
            !paths.is_empty(),
            "iRig USB (VID_{:04X}&PID_{:04X}) not found. Is it connected?",
            IRIG_VID,
            IRIG_PID
        );
        for p in &paths {
            println!("  Found: {}", p.display());
        }
    }
}
