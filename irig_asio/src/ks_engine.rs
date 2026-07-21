//! Direct Kernel Streaming (WDM/KS) Audio Engine for iRig USB.
//!
//! Communicates directly with the `usbaudio.sys` driver handle using
//! `IOCTL_KS_PROPERTY`, `ReadFile`, and `WriteFile` with Overlapped I/O.
//! Bypasses MMDeviceAPI/WASAPI completely for bit-perfect low-latency audio.

#![allow(non_snake_case)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use windows::core::{Result as WinResult, GUID};
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
use windows::Win32::System::IO::{DeviceIoControl, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};

use crate::buffer_manager::BufferManager;
use crate::constants::NUM_OUTPUT_CHANNELS;
use crate::ks_device::{open_ks_pin, PinDirection};

const KSPROPSETID_CONNECTION: GUID = GUID::from_values(
    0x1D58C920,
    0xAC9B,
    0x11CF,
    [0xA5, 0xD6, 0x28, 0xDB, 0x04, 0xC1, 0x00, 0x00],
);

const KSPROPERTY_CONNECTION_STATE: u32 = 5;
const IOCTL_KS_PROPERTY: u32 = 0x002F0003;

pub const KSSTATE_STOP: u32 = 0;
pub const KSSTATE_ACQUIRE: u32 = 1;
pub const KSSTATE_PAUSE: u32 = 2;
pub const KSSTATE_RUN: u32 = 3;

#[repr(C)]
struct KsProperty {
    set: GUID,
    id: u32,
    flags: u32,
}

/// Set pin streaming state (RUN, PAUSE, STOP).
pub fn set_pin_state(handle: HANDLE, state: u32) -> WinResult<()> {
    let prop = KsProperty {
        set: KSPROPSETID_CONNECTION,
        id: KSPROPERTY_CONNECTION_STATE,
        flags: 2, // KSPROPERTY_TYPE_SET
    };

    let mut ks_state = state;
    let mut returned = 0u32;

    unsafe {
        DeviceIoControl(
            handle,
            IOCTL_KS_PROPERTY,
            Some(&prop as *const _ as *const c_void),
            std::mem::size_of::<KsProperty>() as u32,
            Some(&mut ks_state as *mut _ as *mut c_void),
            std::mem::size_of::<u32>() as u32,
            Some(&mut returned),
            None,
        )?;
    }

    log::info!("[KS] Set pin state: {}", state);
    Ok(())
}

/// Active Kernel Streaming session.
pub struct KsAudioStream {
    running: Arc<AtomicBool>,
    sample_count: Arc<AtomicU64>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl KsAudioStream {
    pub fn start(
        device_path: std::path::PathBuf,
        sample_rate: u32,
        buffer_frames: usize,
        buffer_manager: Arc<BufferManager>,
        callbacks: *const crate::asio_driver::AsioCallbacks,
    ) -> WinResult<Self> {
        let running = Arc::new(AtomicBool::new(true));
        let sample_count = Arc::new(AtomicU64::new(0));

        let running_clone = running.clone();
        let sample_count_clone = sample_count.clone();
        let callbacks_addr = callbacks as usize;

        let thread_handle = thread::Builder::new()
            .name("irig-ks-io".into())
            .spawn(move || {
                let cb_ptr = callbacks_addr as *const crate::asio_driver::AsioCallbacks;
                if let Err(e) = ks_io_loop(
                    device_path,
                    sample_rate,
                    buffer_frames,
                    buffer_manager,
                    cb_ptr,
                    running_clone,
                    sample_count_clone,
                ) {
                    log::error!("[KS] Streaming loop error: {:?}", e);
                }
            })
            .expect("Failed to spawn KS streaming thread");

        Ok(Self {
            running,
            sample_count,
            thread_handle: Some(thread_handle),
        })
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(h) = self.thread_handle.take() {
            let _ = h.join();
        }
    }

    pub fn get_sample_position(&self) -> u64 {
        self.sample_count.load(Ordering::Relaxed)
    }
}

/// Core direct Kernel Streaming I/O loop.
fn ks_io_loop(
    device_path: std::path::PathBuf,
    _sample_rate: u32,
    buffer_frames: usize,
    buffer_manager: Arc<BufferManager>,
    callbacks: *const crate::asio_driver::AsioCallbacks,
    running: Arc<AtomicBool>,
    sample_count: Arc<AtomicU64>,
) -> WinResult<()> {
    log::info!("[KS] Starting direct KS streaming loop on {:?}", device_path);

    // Open render and capture pins
    let render_pin = open_ks_pin(&device_path, PinDirection::Render)?;
    let capture_pin = open_ks_pin(&device_path, PinDirection::Capture)?;

    // Transition pins to RUN state
    let _ = set_pin_state(render_pin.handle, KSSTATE_RUN);
    let _ = set_pin_state(capture_pin.handle, KSSTATE_RUN);

    // Audio format: 2 channels, 24-bit PCM in 32-bit container (4 bytes per sample)
    let bytes_per_sample = 4usize;
    let period_bytes = buffer_frames * (NUM_OUTPUT_CHANNELS as usize) * bytes_per_sample;

    let mut cap_raw_buf = vec![0u8; period_bytes];
    let mut ren_raw_buf = vec![0u8; period_bytes];

    // Create overlapped handles
    let cap_event = unsafe { CreateEventW(None, true, false, None)? };
    let ren_event = unsafe { CreateEventW(None, true, false, None)? };

    let mut cap_ov = OVERLAPPED {
        hEvent: cap_event,
        ..Default::default()
    };
    let mut ren_ov = OVERLAPPED {
        hEvent: ren_event,
        ..Default::default()
    };

    buffer_manager.set_running(true);

    while running.load(Ordering::Acquire) {
        // 1. Submit ReadFile on Capture Pin (Guitar Input)
        let read_res = unsafe {
            ReadFile(
                capture_pin.handle,
                Some(&mut cap_raw_buf),
                None,
                Some(&mut cap_ov),
            )
        };

        // 2. Wait for capture completion or tick
        if read_res.is_err() {
            unsafe {
                let err = GetLastError();
                if err.0 == 997 { // ERROR_IO_PENDING
                    let _ = WaitForSingleObject(cap_event, 50);
                    let mut transferred = 0u32;
                    let _ = GetOverlappedResult(capture_pin.handle, &cap_ov, &mut transferred, true);
                }
            }
        }

        if !running.load(Ordering::Acquire) {
            break;
        }

        let new_idx = buffer_manager.swap_index();

        // 3. Unpack 24-bit PCM bytes into ASIO f32 float input buffers
        let ch0_ptr = buffer_manager.input_buffers[0].ptr(new_idx) as *mut f32;
        let ch1_ptr = buffer_manager.input_buffers[1].ptr(new_idx) as *mut f32;

        let src_pcm = unsafe {
            std::slice::from_raw_parts(cap_raw_buf.as_ptr() as *const i32, buffer_frames * 2)
        };

        for i in 0..buffer_frames {
            let left_i32  = src_pcm[i * 2];
            let right_i32 = src_pcm[i * 2 + 1];

            // Convert 24-bit container to f32 float (-1.0 .. +1.0)
            unsafe {
                *ch0_ptr.add(i) = (left_i32 as f32) / 2147483648.0;
                *ch1_ptr.add(i) = (right_i32 as f32) / 2147483648.0;
            }
        }

        // 4. Sample counter increment
        sample_count.fetch_add(buffer_frames as u64, Ordering::Relaxed);

        // 5. Trigger ASIO Host bufferSwitch callback
        if !callbacks.is_null() {
            unsafe {
                let cb_ref = &*callbacks;
                (cb_ref.buffer_switch)(new_idx, crate::asio_driver::ASIO_TRUE);
            }
        }

        // 6. Pack ASIO f32 float output buffers into 24-bit PCM bytes
        let ch0_out = buffer_manager.output_buffers[0].ptr(new_idx) as *const f32;
        let ch1_out = buffer_manager.output_buffers[1].ptr(new_idx) as *const f32;

        let dst_pcm = unsafe {
            std::slice::from_raw_parts_mut(ren_raw_buf.as_mut_ptr() as *mut i32, buffer_frames * 2)
        };

        for i in 0..buffer_frames {
            unsafe {
                let left_f  = *ch0_out.add(i);
                let right_f = *ch1_out.add(i);

                dst_pcm[i * 2]     = (left_f * 2147483647.0) as i32;
                dst_pcm[i * 2 + 1] = (right_f * 2147483647.0) as i32;
            }
        }

        // 7. Submit WriteFile on Render Pin (Headphone Output)
        let write_res = unsafe {
            WriteFile(
                render_pin.handle,
                Some(&ren_raw_buf),
                None,
                Some(&mut ren_ov),
            )
        };

        if write_res.is_err() {
            unsafe {
                let err = GetLastError();
                if err.0 == 997 { // ERROR_IO_PENDING
                    let _ = WaitForSingleObject(ren_event, 50);
                    let mut transferred = 0u32;
                    let _ = GetOverlappedResult(render_pin.handle, &ren_ov, &mut transferred, true);
                }
            }
        }
    }

    // Stop pins
    let _ = set_pin_state(render_pin.handle, KSSTATE_STOP);
    let _ = set_pin_state(capture_pin.handle, KSSTATE_STOP);

    unsafe {
        let _ = CloseHandle(cap_event);
        let _ = CloseHandle(ren_event);
    }

    log::info!("[KS] Streaming loop finished");
    Ok(())
}
