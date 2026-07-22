//! Core ASIO driver implementation — `IrigAsioDriver`.
//!
//! This struct implements the Steinberg ASIO 2.3 interface as a COM object.
//! The COM vtable is constructed manually (Rust has no native COM class
//! syntax) and stored in a static.  Each ASIO method delegates to the safe
//! Rust impl below.
//!
//! ASIO interface summary (methods we implement)
//! ──────────────────────────────────────────────
//!  init()                 → open KS device, negotiate format
//!  getDriverName()        → "iRig USB ASIO (Rust)"
//!  getDriverVersion()     → 1
//!  getErrorMessage()      → last error string
//!  start()                → begin streaming thread
//!  stop()                 → join streaming thread
//!  getChannels()          → 2 in / 2 out
//!  getLatencies()         → buffer size in frames
//!  getBufferSize()        → min/max/preferred/granularity
//!  canSampleRate()        → 44100 / 48000 only
//!  getSampleRate()        → current rate
//!  setSampleRate()        → change rate (must call stop/start)
//!  getClockSources()      → internal clock only
//!  setClockSource()       → noop
//!  getSamplePosition()    → sample counter
//!  getChannelInfo()       → name / type / active flag
//!  createBuffers()        → allocate double-buffers, store host callbacks
//!  disposeBuffers()       → free buffers
//!  controlPanel()         → future: settings dialog
//!  future()               → selector dispatch (outputReady etc.)
//!  outputReady()          → signal buffer manager

#![allow(non_snake_case, clippy::missing_safety_doc)]

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use crate::buffer_manager::BufferManager;
use crate::constants::*;
use crate::ks_device::{
    enumerate_irig_device_paths, get_irig_capture_path,
};

// ---------------------------------------------------------------------------
// ASIO type aliases (until bindgen runs over the real SDK)
// ---------------------------------------------------------------------------

pub type ASIOBool     = i32;
pub type ASIOSamples  = i64;
pub type ASIOSampleRate = f64;

pub const ASIO_TRUE:  ASIOBool = 1;
pub const ASIO_FALSE: ASIOBool = 0;

/// Subset of ASIOChannelInfo used in getChannelInfo().
#[repr(C)]
#[derive(Debug, Clone, Default)]
pub struct AsioChannelInfo {
    pub channel:    i32,
    pub is_input:   ASIOBool,
    pub is_active:  ASIOBool,
    pub group:      i32,
    pub sample_type: i32,
    pub name:       [u8; 32],
}

/// ASIO buffer info passed to createBuffers().
#[repr(C)]
pub struct AsioBufferInfo {
    pub is_input:  ASIOBool,
    pub channel_num: i32,
    /// Filled by the driver: pointers to double-buffers [0] and [1].
    pub buffers:   [*mut c_void; 2],
}

/// Callbacks the host provides in createBuffers().
#[repr(C)]
pub struct AsioCallbacks {
    pub buffer_switch:         unsafe extern "C" fn(double_buffer_index: i32, direct_process: ASIOBool),
    pub sample_rate_did_change: unsafe extern "C" fn(s_rate: ASIOSampleRate),
    pub asio_message:          unsafe extern "C" fn(selector: i32, value: i32, message: *mut c_void, opt: *mut f64) -> i32,
    pub buffer_switch_time_info: unsafe extern "C" fn(params: *mut c_void, double_buffer_index: i32, direct_process: ASIOBool) -> *mut c_void,
}

// ---------------------------------------------------------------------------
// Driver state
// ---------------------------------------------------------------------------

/// Internal state of the iRig ASIO driver.
pub struct DriverState {
    /// KS device path (populated in init()).
    pub device_path: Option<std::path::PathBuf>,

    /// Current sample rate.
    pub sample_rate: ASIOSampleRate,

    /// Current buffer size in frames.
    pub buffer_frames: usize,

    /// Double-buffer manager (present after createBuffers).
    pub buffers: Option<Arc<BufferManager>>,

    /// Host callbacks (present after createBuffers).
    pub callbacks: Option<*const AsioCallbacks>,

    /// Active WASAPI audio stream engine.
    pub wasapi_stream: Option<crate::wasapi_backend::WasapiStream>,

    /// Last error message (returned by getErrorMessage).
    pub last_error: String,

    /// True after init() succeeds.
    pub initialized: bool,
}

impl DriverState {
    pub fn new() -> Self {
        Self {
            device_path: None,
            sample_rate: DEFAULT_SAMPLE_RATE,
            buffer_frames: BUFFER_SIZE_PREFERRED as usize,
            buffers: None,
            callbacks: None,
            wasapi_stream: None,
            last_error: String::new(),
            initialized: false,
        }
    }
}

// SAFETY: DriverState is only accessed through a Mutex<DriverState> inside an Arc.
unsafe impl Send for DriverState {}

// ---------------------------------------------------------------------------
// IrigAsioDriver
// ---------------------------------------------------------------------------

/// The main ASIO driver object.  One instance is created per `CoCreateInstance`
/// call from the DAW.
pub struct IrigAsioDriver {
    state: Arc<Mutex<DriverState>>,
}

impl IrigAsioDriver {
    pub fn new() -> Self {
        init_logger();
        log::info!("[ASIO] IrigAsioDriver created");
        Self {
            state: Arc::new(Mutex::new(DriverState::new())),
        }
    }

    // -----------------------------------------------------------------------
    // ASIO interface methods (safe Rust wrappers called from the COM vtable)
    // -----------------------------------------------------------------------

    /// `init(sysRef)` — Called once by the host after CoCreateInstance.
    /// Opens the KS audio pin and negotiates the PCM format.
    pub fn asio_init(&self, _sys_ref: *mut c_void) -> ASIOBool {
        log::info!("[ASIO] init()");
        let mut st = self.state.lock().unwrap();

        match enumerate_irig_device_paths() {
            Ok(paths) if !paths.is_empty() => {
                st.device_path = Some(paths[0].clone());
                st.initialized = true;
                log::info!("[ASIO] init() OK — device at {:?}", paths[0]);
                ASIO_TRUE
            }
            Ok(_) => {
                st.last_error = format!(
                    "iRig USB (VID_{:04X}/PID_{:04X}) not found. Is it connected?",
                    IRIG_VID, IRIG_PID
                );
                log::warn!("[ASIO] {}", st.last_error);
                ASIO_FALSE
            }
            Err(e) => {
                st.last_error = format!("Device enumeration error: {e}");
                log::error!("[ASIO] {}", st.last_error);
                ASIO_FALSE
            }
        }
    }

    pub fn asio_get_driver_name(&self, name: &mut [u8; 32]) {
        let bytes = DRIVER_NAME.as_bytes();
        let len = bytes.len().min(31);
        name[..len].copy_from_slice(&bytes[..len]);
        name[len] = 0;
    }

    pub fn asio_get_driver_version(&self) -> i32 {
        DRIVER_VERSION
    }

    pub fn asio_get_error_message(&self, msg: &mut [u8; 128]) {
        let st = self.state.lock().unwrap();
        let bytes = st.last_error.as_bytes();
        let len = bytes.len().min(127);
        msg[..len].copy_from_slice(&bytes[..len]);
        msg[len] = 0;
    }

    /// `start()` — Launch the WASAPI audio streaming engine.
    pub fn asio_start(&self) -> i32 {
        log::info!("[ASIO] start()");
        let mut st = self.state.lock().unwrap();

        if !st.initialized {
            st.last_error = "start() called before init()".into();
            return ASE_HW_FAILURE;
        }
        if st.buffers.is_none() || st.callbacks.is_none() {
            st.last_error = "start() called before createBuffers()".into();
            return ASE_HW_FAILURE;
        }

        let sample_rate = st.sample_rate as u32;
        let buffer_frames = st.buffer_frames;
        let buffers = st.buffers.as_ref().unwrap().clone();
        let callbacks = st.callbacks.unwrap();

        // Probe the KS capture path so the WASAPI backend can fall back to
        // direct KS streaming when the iRig USB has no WASAPI capture endpoint.
        let ks_cap_path = get_irig_capture_path();
        if let Some(ref p) = ks_cap_path {
            log::info!("[ASIO] KS capture path: {:?}", p);
        } else {
            log::warn!("[ASIO] No KS capture path found for iRig USB");
        }

        match crate::wasapi_backend::WasapiStream::start(
            sample_rate,
            buffer_frames,
            buffers,
            callbacks,
            ks_cap_path,
        ) {
            Ok(stream) => {
                st.wasapi_stream = Some(stream);
                log::info!("[ASIO] WASAPI audio stream started");
                ASE_OK
            }
            Err(e) => {
                st.last_error = format!("Failed to start WASAPI stream: {:?}", e);
                log::error!("[ASIO] {}", st.last_error);
                ASE_HW_FAILURE
            }
        }
    }

    /// `stop()` — Stop the WASAPI audio streaming engine.
    pub fn asio_stop(&self) -> i32 {
        log::info!("[ASIO] stop()");
        let mut st = self.state.lock().unwrap();
        if let Some(mut stream) = st.wasapi_stream.take() {
            drop(st);
            stream.stop();
        }
        ASE_OK
    }

    pub fn asio_get_channels(&self, num_input: &mut i32, num_output: &mut i32) -> i32 {
        *num_input  = NUM_INPUT_CHANNELS;
        *num_output = NUM_OUTPUT_CHANNELS;
        ASE_OK
    }

    pub fn asio_get_latencies(&self, input_latency: &mut i32, output_latency: &mut i32) -> i32 {
        let st = self.state.lock().unwrap();
        *input_latency  = st.buffer_frames as i32;
        *output_latency = st.buffer_frames as i32;
        ASE_OK
    }

    pub fn asio_get_buffer_size(
        &self,
        min_size: &mut i32,
        max_size: &mut i32,
        preferred: &mut i32,
        granularity: &mut i32,
    ) -> i32 {
        *min_size    = BUFFER_SIZE_MIN;
        *max_size    = BUFFER_SIZE_MAX;
        *preferred   = BUFFER_SIZE_PREFERRED;
        *granularity = BUFFER_SIZE_GRANULARITY;
        ASE_OK
    }

    pub fn asio_can_sample_rate(&self, sample_rate: ASIOSampleRate) -> i32 {
        if (sample_rate - DEFAULT_SAMPLE_RATE).abs() < 0.5
            || (sample_rate - SAMPLE_RATE_44100).abs() < 0.5
        {
            ASE_OK
        } else {
            ASE_NOT_PRESENT
        }
    }

    pub fn asio_get_sample_rate(&self, sample_rate: &mut ASIOSampleRate) -> i32 {
        let st = self.state.lock().unwrap();
        *sample_rate = st.sample_rate;
        ASE_OK
    }

    pub fn asio_set_sample_rate(&self, sample_rate: ASIOSampleRate) -> i32 {
        if self.asio_can_sample_rate(sample_rate) != ASE_OK {
            return ASE_NOT_PRESENT;
        }
        let mut st = self.state.lock().unwrap();
        st.sample_rate = sample_rate;
        log::info!("[ASIO] Sample rate set to {} Hz", sample_rate);
        ASE_OK
    }

    pub fn asio_get_clock_sources(&self, clocks: *mut [u8; 32], num_sources: &mut i32) -> i32 {
        // Internal clock only
        *num_sources = 1;
        if !clocks.is_null() {
            let name = b"Internal\0";
            unsafe {
                (&mut (*clocks))[..name.len()].copy_from_slice(name);
            }
        }
        ASE_OK
    }

    pub fn asio_set_clock_source(&self, _reference: i32) -> i32 {
        ASE_OK // single clock source, nothing to do
    }

    pub fn asio_get_sample_position(
        &self,
        s_pos: &mut ASIOSamples,
        _time_stamp: &mut ASIOSamples,
    ) -> i32 {
        let st = self.state.lock().unwrap();
        if let Some(ref stream) = st.wasapi_stream {
            *s_pos = stream.get_sample_position() as i64;
        } else {
            *s_pos = 0;
        }
        ASE_OK
    }

    pub fn asio_get_channel_info(&self, info: &mut AsioChannelInfo) -> i32 {
        let is_input = info.is_input != 0;
        let ch = info.channel;
        let max_ch = if is_input {
            NUM_INPUT_CHANNELS
        } else {
            NUM_OUTPUT_CHANNELS
        };

        if ch < 0 || ch >= max_ch {
            return ASE_INVALID_PARAMETER;
        }

        info.is_active   = ASIO_TRUE;
        info.group       = 0;
        info.sample_type = ASIO_SAMPLE_TYPE;

        let label = if is_input {
            format!("iRig In {}", ch + 1)
        } else {
            format!("iRig Out {}", ch + 1)
        };
        let bytes = label.as_bytes();
        let len   = bytes.len().min(31);
        info.name[..len].copy_from_slice(&bytes[..len]);
        info.name[len] = 0;

        ASE_OK
    }

    /// `createBuffers()` — Allocate double-buffers and store host callbacks.
    pub fn asio_create_buffers(
        &self,
        buffer_infos: *mut AsioBufferInfo,
        num_channels: i32,
        buffer_size: i32,
        callbacks: *const AsioCallbacks,
    ) -> i32 {
        log::info!("[ASIO] createBuffers({} channels, {} frames)", num_channels, buffer_size);
        let mut st = self.state.lock().unwrap();

        if buffer_size < BUFFER_SIZE_MIN || buffer_size > BUFFER_SIZE_MAX {
            st.last_error = format!("Invalid buffer size: {}", buffer_size);
            return ASE_INVALID_PARAMETER;
        }

        let frames = buffer_size as usize;
        st.buffer_frames = frames;

        let bm = if let Some(existing_bm) = st.buffers.as_ref().filter(|b| b.frames == frames) {
            log::info!("[ASIO] Reusing existing BufferManager of size {}!", frames);
            existing_bm.clone()
        } else {
            match BufferManager::new(frames) {
                Ok(b) => Arc::new(b),
                Err(e) => {
                    st.last_error = format!("Buffer allocation failed: {e}");
                    return ASE_NO_MEMORY;
                }
            }
        };

        // Fill in the buffer pointers the host needs.
        if !buffer_infos.is_null() {
            for i in 0..num_channels as usize {
                let info = unsafe { &mut *buffer_infos.add(i) };
                let is_input = info.is_input != 0;
                let ch       = info.channel_num as usize;

                let (ptr_a, ptr_b) = if is_input {
                    if ch >= bm.input_buffers.len() { continue; }
                    let base = bm.input_buffers[ch].a.as_ptr() as *mut c_void;
                    let base_b = bm.input_buffers[ch].b.as_ptr() as *mut c_void;
                    (base, base_b)
                } else {
                    if ch >= bm.output_buffers.len() { continue; }
                    let base = bm.output_buffers[ch].a.as_ptr() as *mut c_void;
                    let base_b = bm.output_buffers[ch].b.as_ptr() as *mut c_void;
                    (base, base_b)
                };

                info.buffers[0] = ptr_a;
                info.buffers[1] = ptr_b;
            }
        }

        st.buffers   = Some(bm);
        st.callbacks = Some(callbacks);
        log::info!("[ASIO] createBuffers() OK");
        ASE_OK
    }

    pub fn asio_dispose_buffers(&self) -> i32 {
        log::info!("[ASIO] disposeBuffers()");
        let mut st = self.state.lock().unwrap();
        st.buffers   = None;
        st.callbacks = None;
        ASE_OK
    }

    pub fn asio_control_panel(&self) -> i32 {
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
        use windows::core::PCWSTR;

        log::info!("[ASIO] controlPanel() called");
        let st = self.state.lock().unwrap();

        let msg = format!(
            "iRig USB ASIO Driver (Rust) v0.1.0\n\n\
            Current Settings:\n\
            • Sample Rate: {} Hz\n\
            • Audio Buffer: {} frames ({:.2} ms)\n\
            • Input Channels: 2 (iRig Guitar In 1 & 2)\n\
            • Output Channels: 2 (iRig Headphone Out 1 & 2)\n\n\
            Status: Active & Operational",
            st.sample_rate,
            st.buffer_frames,
            (st.buffer_frames as f64 / st.sample_rate) * 1000.0
        );

        let title_wide: Vec<u16> = "iRig USB ASIO Control Panel\0".encode_utf16().collect();
        let msg_wide: Vec<u16> = msg.encode_utf16().chain(std::iter::once(0)).collect();

        unsafe {
            MessageBoxW(
                None,
                PCWSTR(msg_wide.as_ptr()),
                PCWSTR(title_wide.as_ptr()),
                MB_OK | MB_ICONINFORMATION,
            );
        }

        ASE_OK
    }

    /// Called by the host after it has finished writing output samples.
    /// We signal the KS I/O thread to proceed with DMA.
    pub fn asio_output_ready(&self) -> i32 {
        let st = self.state.lock().unwrap();
        if let Some(ref bm) = st.buffers {
            bm.signal_asio_ready();
        }
        ASE_OK
    }
}

// ---------------------------------------------------------------------------
// Logger initialisation (writes to %TEMP%\irig_asio.log)
// ---------------------------------------------------------------------------

fn init_logger() {
    use simplelog::*;
    use std::fs::OpenOptions;

    let log_path = std::env::temp_dir().join("irig_asio.log");
    if let Ok(file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = WriteLogger::init(
            LevelFilter::Debug,
            Config::default(),
            file,
        );
    }
}
