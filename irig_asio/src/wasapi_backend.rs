//! WASAPI audio engine backend for iRig USB ASIO Driver.
//!
//! Capture path priority
//! ^u2500^u2500^u2500^u2500^u2500^u2500^u2500^u2500^u2500^u2500^u2500^u2500^u2500^u2500^u2500^u2500^u2500
//!  1. User-configured preferred device (config file)
//!  2. WASAPI: any endpoint (all states) matching VID_1963 / irig
//!  3. KS direct capture: KSCATEGORY_CAPTURE path for VID_1963
//!     (used when the iRig USB does not expose a WASAPI capture endpoint)
//!  4. Silence (zeros) -- DAW still runs, just no guitar input.

#![allow(non_snake_case)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use windows::core::{Result as WinResult, GUID};
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};
use windows::Win32::UI::Shell::PropertiesSystem::*;

use crate::buffer_manager::BufferManager;
use crate::ks_device::{get_irig_capture_path, ks_transition_to_run, open_ks_pin, KsPin, PinDirection};

const WAVE_FORMAT_IEEE_FLOAT: u32 = 0x0003;
const WAVE_FORMAT_EXTENSIBLE: u32 = 0xFFFE;

const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_values(
        0xa45c254e, 0xdf1c, 0x4efd,
        [0x80, 0x20, 0x67, 0xd1, 0x46, 0xa8, 0x50, 0xe0],
    ),
    pid: 14,
};

const PKEY_DEVICE_INTERFACE_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_values(
        0x505e9c50, 0xa227, 0x4e80,
        [0x99, 0x3a, 0xb6, 0x14, 0xe6, 0x96, 0x61, 0x44],
    ),
    pid: 0,
};

// ---------------------------------------------------------------------------
// KS capture state (used when WASAPI capture is unavailable)
// ---------------------------------------------------------------------------

struct KsCaptureState {
    pin: KsPin,
    event: windows::Win32::Foundation::HANDLE,
    buf: Vec<u8>,
    header: Box<windows::Win32::Media::KernelStreaming::KSSTREAM_HEADER>,
    overlapped: Box<OVERLAPPED>,
    pending: bool,
}

impl Drop for KsCaptureState {
    fn drop(&mut self) {
        unsafe { let _ = CloseHandle(self.event); }
    }
}

impl KsCaptureState {
    unsafe fn try_open(path: &std::path::Path, buf_frames: usize) -> Option<Self> {
        let pin = open_ks_pin(&path.to_path_buf(), PinDirection::Capture).ok()?;
        ks_transition_to_run(&pin);
        let event = CreateEventW(None, true, false, None).ok()?;
        let bytes_per_frame = (pin.block_align as usize).max(2);
        let buf_size = buf_frames * bytes_per_frame;
        let mut state = KsCaptureState {
            pin,
            event,
            buf: vec![0u8; buf_size],
            header: Box::new(windows::Win32::Media::KernelStreaming::KSSTREAM_HEADER::default()),
            overlapped: Box::new(OVERLAPPED::default()),
            pending: false,
        };
        state.overlapped.hEvent = event;
        state.submit_read();
        Some(state)
    }

    unsafe fn submit_read(&mut self) {
        use windows::Win32::System::IO::DeviceIoControl;
        use windows::Win32::Media::KernelStreaming::{IOCTL_KS_READ_STREAM, KSSTREAM_HEADER, KSTIME};

        self.header.Size = std::mem::size_of::<KSSTREAM_HEADER>() as u32;
        self.header.TypeSpecificFlags = 0;
        self.header.PresentationTime = KSTIME::default();
        self.header.Duration = 0;
        self.header.FrameExtent = self.buf.len() as u32;
        self.header.DataUsed = 0;
        self.header.Data = self.buf.as_mut_ptr() as *mut std::ffi::c_void;
        self.header.OptionsFlags = 0;
        self.header.Reserved = 0;

        let mut bytes_returned = 0u32;
        let result = DeviceIoControl(
            self.pin.handle,
            IOCTL_KS_READ_STREAM,
            Some(std::ptr::addr_of_mut!(*self.header) as *mut std::ffi::c_void),
            std::mem::size_of::<KSSTREAM_HEADER>() as u32,
            Some(std::ptr::addr_of_mut!(*self.header) as *mut std::ffi::c_void),
            std::mem::size_of::<KSSTREAM_HEADER>() as u32,
            Some(&mut bytes_returned),
            Some(&mut *self.overlapped),
        );

        let pending_hresult = windows::core::HRESULT(0x800703E5_u32 as i32);
        match result {
            Ok(_) => { self.pending = true; }
            Err(ref e) if e.code() == pending_hresult => { self.pending = true; }
            Err(ref e) => {
                log::warn!("[KsCapture] IOCTL_KS_READ_STREAM error: {:?}", e);
                self.pending = false;
            }
        }
    }

    unsafe fn try_consume(&mut self, buf_frames: usize, ch0: *mut f32, ch1: *mut f32) -> bool {
        if !self.pending { return false; }
        let mut bytes_xferred = 0u32;
        if GetOverlappedResult(self.pin.handle, &mut *self.overlapped, &mut bytes_xferred, true).is_err() {
            return false;
        }
        self.pending = false;

        let data_bytes = self.header.DataUsed as usize;
        if data_bytes == 0 {
            let _ = ResetEvent(self.event);
            self.submit_read();
            return false;
        }

        // Calculate sample bytes based on actual data received
        let sample_bytes = if data_bytes >= buf_frames * 3 {
            data_bytes / buf_frames
        } else {
            2
        };
        let frames = (data_bytes / sample_bytes).min(buf_frames);

        if sample_bytes == 2 {
            // 16-bit PCM mono (2 bytes per sample) - standard iRig ADC format
            let src = self.buf.as_ptr() as *const i16;
            for i in 0..frames {
                let v = (*src.add(i)) as f32 / 32768.0;
                *ch0.add(i) = v;
                *ch1.add(i) = v;
            }
        } else if sample_bytes == 3 {
            // 24-bit PCM mono (3 bytes per sample)
            for i in 0..frames {
                let off = i * 3;
                let raw = (self.buf[off] as i32)
                        | ((self.buf[off + 1] as i32) << 8)
                        | ((self.buf[off + 2] as i32) << 16);
                let signed_val = if raw & 0x800000 != 0 { raw | !0xFFFFFF } else { raw };
                let v = signed_val as f32 / 8_388_608.0;
                *ch0.add(i) = v;
                *ch1.add(i) = v;
            }
        } else if sample_bytes >= 4 {
            // 32-bit or 24-bit in 32-bit container PCM mono
            let src = self.buf.as_ptr() as *const i32;
            for i in 0..frames {
                let v = (*src.add(i)) as f32 / 2_147_483_648.0;
                *ch0.add(i) = v;
                *ch1.add(i) = v;
            }
        }

        for i in frames..buf_frames {
            *ch0.add(i) = 0.0;
            *ch1.add(i) = 0.0;
        }

        let _ = ResetEvent(self.event);
        self.submit_read();
        true
    }
}

struct KsRenderState {
    pin: KsPin,
    event: windows::Win32::Foundation::HANDLE,
    buf: Vec<u8>,
    header: Box<windows::Win32::Media::KernelStreaming::KSSTREAM_HEADER>,
    overlapped: Box<OVERLAPPED>,
    pending: bool,
}

impl Drop for KsRenderState {
    fn drop(&mut self) {
        unsafe { let _ = CloseHandle(self.event); }
    }
}

impl KsRenderState {
    unsafe fn try_open(path: &std::path::Path, buf_frames: usize) -> Option<Self> {
        let pin = open_ks_pin(&path.to_path_buf(), PinDirection::Render).ok()?;
        ks_transition_to_run(&pin);
        let event = CreateEventW(None, true, false, None).ok()?;
        let buf_size = buf_frames * 2 * 2;
        let mut state = KsRenderState {
            pin,
            event,
            buf: vec![0u8; buf_size],
            header: Box::new(windows::Win32::Media::KernelStreaming::KSSTREAM_HEADER::default()),
            overlapped: Box::new(OVERLAPPED::default()),
            pending: false,
        };
        state.overlapped.hEvent = event;
        log::info!("[KsRender] Direct KS Render output ACTIVE on Pin #0 (iRig USB DAC)");
        Some(state)
    }

    unsafe fn write_samples(&mut self, buf_frames: usize, ch0: *const f32, ch1: *const f32) {
        use windows::Win32::System::IO::{DeviceIoControl, GetOverlappedResult};
        use windows::Win32::Media::KernelStreaming::{IOCTL_KS_WRITE_STREAM, KSSTREAM_HEADER, KSTIME};

        if self.pending {
            let mut xfer = 0u32;
            let _ = GetOverlappedResult(self.pin.handle, &mut *self.overlapped, &mut xfer, true);
            self.pending = false;
        }

        let dst = self.buf.as_mut_ptr() as *mut i16;
        for i in 0..buf_frames {
            let l = ((*ch0.add(i)).clamp(-1.0, 1.0) * 32767.0) as i16;
            let r = ((*ch1.add(i)).clamp(-1.0, 1.0) * 32767.0) as i16;
            *dst.add(i * 2) = l;
            *dst.add(i * 2 + 1) = r;
        }

        self.header.Size = std::mem::size_of::<KSSTREAM_HEADER>() as u32;
        self.header.TypeSpecificFlags = 0;
        self.header.PresentationTime = KSTIME::default();
        self.header.Duration = 0;
        self.header.FrameExtent = (buf_frames * 4) as u32;
        self.header.DataUsed = (buf_frames * 4) as u32;
        self.header.Data = self.buf.as_mut_ptr() as *mut std::ffi::c_void;
        self.header.OptionsFlags = 0;
        self.header.Reserved = 0;

        let mut bytes_returned = 0u32;
        let _ = DeviceIoControl(
            self.pin.handle,
            IOCTL_KS_WRITE_STREAM,
            Some(std::ptr::addr_of_mut!(*self.header) as *mut std::ffi::c_void),
            std::mem::size_of::<KSSTREAM_HEADER>() as u32,
            Some(std::ptr::addr_of_mut!(*self.header) as *mut std::ffi::c_void),
            std::mem::size_of::<KSSTREAM_HEADER>() as u32,
            Some(&mut bytes_returned),
            Some(&mut *self.overlapped),
        );
        self.pending = true;
    }
}

// ---------------------------------------------------------------------------
// WasapiStream
// ---------------------------------------------------------------------------

pub struct WasapiStream {
    running: Arc<AtomicBool>,
    sample_count: Arc<AtomicU64>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl WasapiStream {
    pub fn start(
        sample_rate: u32,
        buffer_frames: usize,
        buffer_manager: Arc<BufferManager>,
        callbacks: *const crate::asio_driver::AsioCallbacks,
        ks_capture_path: Option<std::path::PathBuf>,
    ) -> WinResult<Self> {
        let running = Arc::new(AtomicBool::new(true));
        let sample_count = Arc::new(AtomicU64::new(0));
        let running_clone = running.clone();
        let sample_count_clone = sample_count.clone();
        let callbacks_addr = callbacks as usize;
        let ks_path_string = ks_capture_path.map(|p| p.to_string_lossy().into_owned());

        let thread_handle = thread::Builder::new()
            .name("irig-wasapi-io".into())
            .spawn(move || {
                let cb_ptr = callbacks_addr as *const crate::asio_driver::AsioCallbacks;
                let ks_path = ks_path_string.map(std::path::PathBuf::from);
                if let Err(e) = wasapi_io_loop(
                    sample_rate, buffer_frames, buffer_manager,
                    cb_ptr, running_clone, sample_count_clone, ks_path,
                ) {
                    log::error!("[WASAPI] IO loop error: {:?}", e);
                }
            })
            .expect("Failed to spawn WASAPI I/O thread");

        Ok(Self { running, sample_count, thread_handle: Some(thread_handle) })
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(h) = self.thread_handle.take() { let _ = h.join(); }
    }

    pub fn get_sample_position(&self) -> u64 {
        self.sample_count.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Endpoint search
// ---------------------------------------------------------------------------

unsafe fn find_endpoint(data_flow: EDataFlow) -> WinResult<IMMDevice> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let flow_str = if data_flow == eCapture { "Capture" } else { "Render" };
    log::info!("[WASAPI] Searching {} endpoint...", flow_str);

    // For RENDER only: check user config preference
    // (We skip this for capture — the config was unreliable and caused Voicemeeter to be used)
    if data_flow != eCapture {
        if let Ok(home) = std::env::var("USERPROFILE") {
            let cfg = std::path::Path::new(&home).join(".irig_asio_config.json");
            if let Ok(content) = std::fs::read_to_string(&cfg) {
                for line in content.lines() {
                    if line.contains("preferred_render_device") {
                        if let Some(pos) = line.find(':') {
                            let pref = line[pos+1..].trim().trim_matches(|c| c == '"' || c == ',' || c == ' ');
                            if !pref.is_empty() {
                                if let Ok(coll) = enumerator.EnumAudioEndpoints(data_flow, DEVICE_STATE_ACTIVE) {
                                    if let Ok(n) = coll.GetCount() {
                                        for i in 0..n {
                                            if let Ok(dev) = coll.Item(i) {
                                                if let Ok(props) = dev.OpenPropertyStore(STGM_READ) {
                                                    if let Ok(v) = props.GetValue(&PKEY_DEVICE_FRIENDLY_NAME) {
                                                        if v.to_string().to_lowercase().contains(&pref.to_lowercase()) {
                                                            log::info!("[WASAPI] Config preferred render: {}", v.to_string());
                                                            return Ok(dev);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Search ACTIVE endpoints matching data_flow direction
    if let Ok(coll) = enumerator.EnumAudioEndpoints(data_flow, DEVICE_STATE_ACTIVE) {
        if let Ok(n) = coll.GetCount() {
            for i in 0..n {
                if let Ok(dev) = coll.Item(i) {
                    if let Ok(props) = dev.OpenPropertyStore(STGM_READ) {
                        let fname = props.GetValue(&PKEY_DEVICE_FRIENDLY_NAME)
                            .map(|v| v.to_string().to_lowercase()).unwrap_or_default();

                        // For RENDER: skip capture endpoints even if they match vid_1963
                        if data_flow == eRender && (fname.contains("microfone") || fname.contains("mic") || fname.contains("input") || fname.contains("entrada")) {
                            continue;
                        }

                        let by_vid = props.GetValue(&PKEY_DEVICE_INTERFACE_ID)
                            .map(|v| v.to_string().to_lowercase().contains("vid_1963"))
                            .unwrap_or(false);
                        let by_name = fname.contains("irig");
                        if by_vid || by_name {
                            let name = props.GetValue(&PKEY_DEVICE_FRIENDLY_NAME)
                                .map(|v| v.to_string()).unwrap_or("iRig USB".into());
                            log::info!("[WASAPI] iRig active match for {}: \"{}\"", flow_str, name);
                            return Ok(dev);
                        }
                    }
                }
            }
        }
    }

    // Search ALL MMDevice states (active + disabled + notpresent + unplugged)
    // for any endpoint matching VID_1963 or containing "irig" in name.
    if let Ok(coll) = enumerator.EnumAudioEndpoints(data_flow, DEVICE_STATE(0xF)) {
        if let Ok(n) = coll.GetCount() {
            for i in 0..n {
                if let Ok(dev) = coll.Item(i) {
                    if let Ok(props) = dev.OpenPropertyStore(STGM_READ) {
                        let fname = props.GetValue(&PKEY_DEVICE_FRIENDLY_NAME)
                            .map(|v| v.to_string().to_lowercase()).unwrap_or_default();

                        // For RENDER: skip capture endpoints even if they match vid_1963
                        if data_flow == eRender && (fname.contains("microfone") || fname.contains("mic") || fname.contains("input") || fname.contains("entrada")) {
                            continue;
                        }

                        let by_vid = props.GetValue(&PKEY_DEVICE_INTERFACE_ID)
                            .map(|v| v.to_string().to_lowercase().contains("vid_1963"))
                            .unwrap_or(false);
                        let by_name = fname.contains("irig");
                        if by_vid || by_name {
                            let name = props.GetValue(&PKEY_DEVICE_FRIENDLY_NAME)
                                .map(|v| v.to_string()).unwrap_or("iRig USB".into());
                            log::info!("[WASAPI] iRig in ALL states for {}: \"{}\"", flow_str, name);
                            return Ok(dev);
                        }
                    }
                }
            }
        }
    }

    // For CAPTURE: if no iRig-specific endpoint found, return Err.
    // This ensures we fall through to KS direct capture.
    // We must NEVER capture from M-Audio, Voicemeeter, or system default
    // because the guitar is connected to the iRig USB, not those devices.
    if data_flow == eCapture {
        log::warn!("[WASAPI] No iRig WASAPI capture endpoint — will use KS direct capture");
        return Err(windows::core::Error::new(
            windows::core::HRESULT(0x80004005_u32 as i32), // E_FAIL
            "No iRig USB WASAPI capture endpoint found",
        ));
    }

    // For RENDER: search specifically for render audio endpoints (Speakers, Headphones, Alto-falantes, iRig output)
    if data_flow == eRender {
        if let Ok(coll) = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) {
            if let Ok(n) = coll.GetCount() {
                for i in 0..n {
                    if let Ok(dev) = coll.Item(i) {
                        if let Ok(props) = dev.OpenPropertyStore(STGM_READ) {
                            if let Ok(v) = props.GetValue(&PKEY_DEVICE_FRIENDLY_NAME) {
                                let name = v.to_string();
                                let l = name.to_lowercase();
                                // Exclude microphones/inputs so we never accidentally select a capture device for render
                                if !l.contains("microfone") && !l.contains("mic") && !l.contains("input") && !l.contains("entrada") {
                                    if l.contains("alto-falantes") || l.contains("speakers") || l.contains("headphone") || l.contains("fone") || l.contains("irig") {
                                        log::info!("[WASAPI] Preferred WASAPI render device found: \"{}\"", name);
                                        return Ok(dev);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        log::info!("[WASAPI] Using system default for render");
        return enumerator.GetDefaultAudioEndpoint(eRender, eConsole);
    }

    log::warn!("[WASAPI] Using system default for capture");
    enumerator.GetDefaultAudioEndpoint(data_flow, eConsole)
}

// ---------------------------------------------------------------------------
// Core IO loop
// ---------------------------------------------------------------------------

fn wasapi_io_loop(
    sample_rate: u32,
    buffer_frames: usize,
    buffer_manager: Arc<BufferManager>,
    callbacks: *const crate::asio_driver::AsioCallbacks,
    running: Arc<AtomicBool>,
    sample_count: Arc<AtomicU64>,
    ks_capture_path: Option<std::path::PathBuf>,
) -> WinResult<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let capture_device = find_endpoint(eCapture).ok();
        let render_device  = find_endpoint(eRender).ok();

        let capture_client: Option<IAudioClient> = capture_device.as_ref().and_then(|d| {
            match d.Activate(CLSCTX_ALL, None) {
                Ok(c) => {
                    log::info!("[WASAPI] Activated WASAPI capture IAudioClient");
                    Some(c)
                }
                Err(e) => {
                    log::warn!("[WASAPI] Failed to activate WASAPI capture IAudioClient: {:?}", e);
                    None
                }
            }
        });
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let render_client: Option<IAudioClient> = render_device.as_ref()
            .and_then(|d| d.Activate(CLSCTX_ALL, None).ok())
            .or_else(|| {
                log::info!("[WASAPI] Retrying render activation with system default endpoint...");
                enumerator.GetDefaultAudioEndpoint(eRender, eConsole)
                    .ok()
                    .and_then(|d| d.Activate(CLSCTX_ALL, None).ok())
            });

        if render_client.is_some() {
            log::info!("[WASAPI] Activated WASAPI render IAudioClient");
        } else {
            log::warn!("[WASAPI] Could not activate any WASAPI render client");
        }

        let event_handle = CreateEventW(None, false, false, None)?;
        let buffer_duration_hns = (buffer_frames as f64 / sample_rate as f64 * 10_000_000.0) as i64;

        let mut cap_bits = 32u16; let mut cap_channels = 2u16; let mut cap_is_float = false;
        if let Some(ref c) = capture_client {
            if let Ok(wfx) = c.GetMixFormat() {
                let w = *wfx;
                cap_bits = w.wBitsPerSample; cap_channels = w.nChannels;
                cap_is_float = cap_bits == 32 || w.wFormatTag == WAVE_FORMAT_IEEE_FLOAT as u16 || w.wFormatTag == WAVE_FORMAT_EXTENSIBLE as u16;
                let _ = c.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, buffer_duration_hns, 0, wfx, None);
                let _ = c.SetEventHandle(event_handle);
                CoTaskMemFree(Some(wfx as _));
            }
        }

        let mut ren_bits = 32u16; let mut ren_channels = 2u16; let mut ren_is_float = false;
        if let Some(ref c) = render_client {
            if let Ok(wfx) = c.GetMixFormat() {
                let w = *wfx;
                ren_bits = w.wBitsPerSample; ren_channels = w.nChannels;
                ren_is_float = ren_bits == 32 || w.wFormatTag == WAVE_FORMAT_IEEE_FLOAT as u16 || w.wFormatTag == WAVE_FORMAT_EXTENSIBLE as u16;
                let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM;
                let _ = c.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, buffer_duration_hns, 0, wfx, None);
                let _ = c.SetEventHandle(event_handle);
                CoTaskMemFree(Some(wfx as _));
            }
        }

        let capture_service: Option<IAudioCaptureClient> = capture_client.as_ref().and_then(|c| { let _ = c.Start(); c.GetService().ok() });
        let render_service:  Option<IAudioRenderClient>  = render_client.as_ref().and_then(|c| { let _ = c.Start(); c.GetService().ok() });

        // KS capture fallback: used when WASAPI has no active capture endpoint for iRig
        let mut ks_cap: Option<KsCaptureState> = if capture_service.is_none() {
            let path = ks_capture_path.clone().or_else(get_irig_capture_path);
            if let Some(ref p) = path {
                log::info!("[WASAPI] WASAPI capture unavailable -- opening KS pin {:?}", p);
                match KsCaptureState::try_open(p.as_path(), buffer_frames) {
                    Some(s) => { log::info!("[WASAPI] KS capture ACTIVE (direct iRig USB ADC read)"); Some(s) }
                    None    => { log::warn!("[WASAPI] KS capture failed; input = silence"); None }
                }
            } else {
                log::warn!("[WASAPI] No KS capture path; input = silence");
                None
            }
        } else { None };

        // KS render fallback: used when WASAPI has no active render endpoint for iRig
        let mut ks_ren: Option<KsRenderState> = if render_service.is_none() {
            let path = ks_capture_path.clone().or_else(get_irig_capture_path);
            if let Some(ref p) = path {
                match KsRenderState::try_open(p.as_path(), buffer_frames) {
                    Some(s) => { log::info!("[WASAPI] KS render ACTIVE (direct iRig USB DAC write)"); Some(s) }
                    None    => { log::warn!("[WASAPI] KS render failed"); None }
                }
            } else { None }
        } else { None };

        // Pre-fill WASAPI render with 1 buffer of silence to establish a jitter cushion
        if let Some(ref ren) = render_service {
            if let Ok(dptr) = ren.GetBuffer(buffer_frames as u32) {
                if !dptr.is_null() {
                    let bytes_per_sample = if ren_is_float { 4 } else { (ren_bits / 8) as usize };
                    std::ptr::write_bytes(dptr as *mut u8, 0, buffer_frames * (ren_channels as usize) * bytes_per_sample);
                    let _ = ren.ReleaseBuffer(buffer_frames as u32, 0);
                }
            }
        }

        buffer_manager.set_running(true);
        let mut consecutive_failures = 0u32;

        while running.load(Ordering::Acquire) {
            let active_event = if let Some(ref ks) = ks_cap {
                ks.event
            } else {
                event_handle
            };

            let wait = WaitForSingleObject(active_event, 20);
            if wait != WAIT_OBJECT_0 && running.load(Ordering::Acquire) {
                thread::sleep(std::time::Duration::from_micros(
                    (1_000_000.0 * buffer_frames as f64 / sample_rate as f64) as u64));
            }
            if !running.load(Ordering::Acquire) { break; }

            let hw_idx = buffer_manager.swap_index();
            let render_idx = 1 - hw_idx;

            // -- WASAPI capture
            if let Some(ref cap) = capture_service {
                let ch0 = buffer_manager.input_buffers[0].ptr(hw_idx) as *mut f32;
                let ch1 = buffer_manager.input_buffers[1].ptr(hw_idx) as *mut f32;
                while let Ok(pkt) = cap.GetNextPacketSize() {
                    if pkt == 0 { break; }
                    let mut dptr = std::ptr::null_mut();
                    let mut frames = 0u32;
                    let mut flags = 0u32;
                    if cap.GetBuffer(&mut dptr, &mut frames, &mut flags, None, None).is_ok() && !dptr.is_null() && frames > 0 {
                        consecutive_failures = 0;
                        let n = (frames as usize).min(buffer_frames);
                        if (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 {
                            for i in 0..n { *ch0.add(i) = 0.0; *ch1.add(i) = 0.0; }
                        } else if cap_channels == 1 {
                            if cap_is_float {
                                let s = std::slice::from_raw_parts(dptr as *const f32, n);
                                for i in 0..n { *ch0.add(i) = s[i]; *ch1.add(i) = s[i]; }
                            } else if cap_bits == 16 {
                                let s = std::slice::from_raw_parts(dptr as *const i16, n);
                                for i in 0..n { let v = s[i] as f32 / 32768.0; *ch0.add(i) = v; *ch1.add(i) = v; }
                            } else {
                                let s = std::slice::from_raw_parts(dptr as *const i32, n);
                                for i in 0..n { let v = s[i] as f32 / 2_147_483_648.0; *ch0.add(i) = v; *ch1.add(i) = v; }
                            }
                        } else {
                            if cap_is_float {
                                let s = std::slice::from_raw_parts(dptr as *const f32, n*2);
                                for i in 0..n { *ch0.add(i) = s[i*2]; *ch1.add(i) = s[i*2+1]; }
                            } else if cap_bits == 16 {
                                let s = std::slice::from_raw_parts(dptr as *const i16, n*2);
                                for i in 0..n { *ch0.add(i) = s[i*2] as f32/32768.0; *ch1.add(i) = s[i*2+1] as f32/32768.0; }
                            } else {
                                let s = std::slice::from_raw_parts(dptr as *const i32, n*2);
                                for i in 0..n { *ch0.add(i) = s[i*2] as f32/2_147_483_648.0; *ch1.add(i) = s[i*2+1] as f32/2_147_483_648.0; }
                            }
                        }
                        let _ = cap.ReleaseBuffer(frames);
                    }
                }
            }

            // -- KS capture fallback
            if capture_service.is_none() {
                if let Some(ref mut ks) = ks_cap {
                    let ch0 = buffer_manager.input_buffers[0].ptr(hw_idx) as *mut f32;
                    let ch1 = buffer_manager.input_buffers[1].ptr(hw_idx) as *mut f32;
                    ks.try_consume(buffer_frames, ch0, ch1);
                }
            }

            if consecutive_failures > 20 {
                log::warn!("[WASAPI] Device lost -- notifying host kAsioResetRequest");
                if !callbacks.is_null() {
                    ((*callbacks).asio_message)(3, 0, std::ptr::null_mut(), std::ptr::null_mut());
                }
                break;
            }

            // -- WASAPI render (drains the finished output buffer render_idx = 1 - hw_idx)
            if let Some(ref ren) = render_service {
                if let Some(ref client) = render_client {
                    let padding = client.GetCurrentPadding().unwrap_or(0);
                    let buffer_size = client.GetBufferSize().unwrap_or(buffer_frames as u32);
                    let available_space = buffer_size.saturating_sub(padding) as usize;
                    let to_write = available_space.min(buffer_frames);
                    if to_write > 0 {
                        if let Ok(dptr) = ren.GetBuffer(to_write as u32) {
                            if !dptr.is_null() {
                                let n = to_write;
                                let ch0 = buffer_manager.output_buffers[0].ptr(render_idx) as *const f32;
                                let ch1 = buffer_manager.output_buffers[1].ptr(render_idx) as *const f32;
                                if ren_channels == 1 {
                                    if ren_is_float {
                                        let d = std::slice::from_raw_parts_mut(dptr as *mut f32, n);
                                        for i in 0..n { d[i] = ((*ch0.add(i) + *ch1.add(i)) * 0.5).clamp(-1.0, 1.0); }
                                    } else if ren_bits == 16 {
                                        let d = std::slice::from_raw_parts_mut(dptr as *mut i16, n);
                                        for i in 0..n { d[i] = (((*ch0.add(i) + *ch1.add(i)) * 0.5).clamp(-1.0, 1.0) * 32767.0) as i16; }
                                    } else {
                                        let d = std::slice::from_raw_parts_mut(dptr as *mut i32, n);
                                        for i in 0..n { d[i] = (((*ch0.add(i) + *ch1.add(i)) * 0.5).clamp(-1.0, 1.0) * 2_147_483_647.0) as i32; }
                                    }
                                } else {
                                    if ren_is_float {
                                        let d = std::slice::from_raw_parts_mut(dptr as *mut f32, n * 2);
                                        for i in 0..n { d[i * 2] = (*ch0.add(i)).clamp(-1.0, 1.0); d[i * 2 + 1] = (*ch1.add(i)).clamp(-1.0, 1.0); }
                                    } else if ren_bits == 16 {
                                        let d = std::slice::from_raw_parts_mut(dptr as *mut i16, n * 2);
                                        for i in 0..n { d[i * 2] = ((*ch0.add(i)).clamp(-1.0, 1.0) * 32767.0) as i16; d[i * 2 + 1] = ((*ch1.add(i)).clamp(-1.0, 1.0) * 32767.0) as i16; }
                                    } else {
                                        let d = std::slice::from_raw_parts_mut(dptr as *mut i32, n * 2);
                                        for i in 0..n { d[i * 2] = ((*ch0.add(i)).clamp(-1.0, 1.0) * 2_147_483_647.0) as i32; d[i * 2 + 1] = ((*ch1.add(i)).clamp(-1.0, 1.0) * 2_147_483_647.0) as i32; }
                                    }
                                }
                                let _ = ren.ReleaseBuffer(to_write as u32, 0);
                            }
                        }
                    }
                }
            } else if let Some(ref mut ks_r) = ks_ren {
                let ch0 = buffer_manager.output_buffers[0].ptr(render_idx) as *const f32;
                let ch1 = buffer_manager.output_buffers[1].ptr(render_idx) as *const f32;
                ks_r.write_samples(buffer_frames, ch0, ch1);
            }

            sample_count.fetch_add(buffer_frames as u64, Ordering::Relaxed);

            if !callbacks.is_null() {
                ((*callbacks).buffer_switch)(hw_idx, crate::asio_driver::ASIO_TRUE);
            }
        }

        if let Some(ref c) = capture_client { let _ = c.Stop(); }
        if let Some(ref c) = render_client  { let _ = c.Stop(); }
        drop(ks_cap);
        let _ = CloseHandle(event_handle);
        log::info!("[WASAPI] Stream loop finished");
    }
    Ok(())
}
