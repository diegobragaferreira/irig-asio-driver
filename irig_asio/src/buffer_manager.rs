//! Double-buffer (ping-pong) manager for the ASIO audio callback loop.
//!
//! ASIO uses a classic double-buffer scheme:
//!
//!   ┌──────────┐   ┌──────────┐
//!   │ Buffer A │   │ Buffer B │
//!   └──────────┘   └──────────┘
//!         ▲               ▲
//!         │               │
//!   DAW fills A     KS drains A
//!   KS drains B     DAW fills B
//!         │               │
//!         └───── swap ────┘
//!
//! The ASIO host (DAW) calls `bufferSwitch(index)` which tells the application
//! which buffer is now ready to fill/drain.  Meanwhile our KS I/O thread is
//! streaming the other buffer to/from the hardware.
//!
//! Thread safety
//! ─────────────
//! The audio callback must NEVER block on a mutex (that would cause xruns).
//! We use an atomic index + Windows Event objects for zero-allocation
//! synchronisation between the DAW callback thread and the KS I/O thread.

use std::sync::atomic::{AtomicI32, AtomicU8, Ordering};
use std::sync::Arc;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{
    CreateEventW, ResetEvent, SetEvent, WaitForSingleObject, INFINITE,
};

use crate::constants::{NUM_INPUT_CHANNELS, NUM_OUTPUT_CHANNELS};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Index of the currently active ASIO buffer (0 or 1).
pub type BufferIndex = i32;

/// One half of a double-buffer pair for a single channel.
#[repr(C, align(64))]
#[derive(Debug)]
pub struct ChannelBuffer {
    /// Raw PCM samples (32-bit words, 24-bit data left-aligned).
    pub data: Vec<i32>,
    raw_ptr: *mut i32,
}

unsafe impl Send for ChannelBuffer {}
unsafe impl Sync for ChannelBuffer {}

impl ChannelBuffer {
    pub fn new(frames: usize) -> Self {
        let mut data = vec![0i32; frames];
        let raw_ptr = data.as_mut_ptr();
        Self { data, raw_ptr }
    }

    pub fn as_ptr(&self) -> *const i32 {
        self.raw_ptr as *const i32
    }

    pub fn as_mut_ptr(&self) -> *mut i32 {
        self.raw_ptr
    }
}

/// Double-buffer pair for one channel (A + B).
#[repr(C, align(64))]
pub struct DoubleBuffer {
    pub a: ChannelBuffer,
    pub b: ChannelBuffer,
}

impl DoubleBuffer {
    pub fn new(frames: usize) -> Self {
        Self {
            a: ChannelBuffer::new(frames),
            b: ChannelBuffer::new(frames),
        }
    }

    /// Return a raw pointer to the requested buffer half.
    pub fn ptr(&self, index: BufferIndex) -> *mut i32 {
        match index {
            0 => self.a.as_mut_ptr(),
            _ => self.b.as_mut_ptr(),
        }
    }
}

// ---------------------------------------------------------------------------
// BufferManager
// ---------------------------------------------------------------------------

/// Manages all ASIO double-buffers and inter-thread synchronisation events.
pub struct BufferManager {
    /// Number of frames per buffer.
    pub frames: usize,

    // Input channel double-buffers (one per channel).
    pub input_buffers: Vec<DoubleBuffer>,

    // Output channel double-buffers (one per channel).
    pub output_buffers: Vec<DoubleBuffer>,

    /// Which buffer index the ASIO host is currently processing (0 or 1).
    pub active_index: Arc<AtomicI32>,

    /// Set by the KS I/O thread when a new hardware buffer is ready.
    pub hw_ready_event: SafeHandle,

    /// Set by the ASIO callback (outputReady) to signal the KS thread.
    pub asio_ready_event: SafeHandle,

    /// Non-zero while streaming is active.
    pub running: Arc<AtomicU8>,
}

impl BufferManager {
    /// Allocate all buffers for the requested frame count.
    pub fn new(frames: usize) -> windows::core::Result<Self> {
        let n_in = NUM_INPUT_CHANNELS as usize;
        let n_out = NUM_OUTPUT_CHANNELS as usize;

        let mut input_buffers = Vec::with_capacity(n_in);
        for _ in 0..n_in {
            input_buffers.push(DoubleBuffer::new(frames));
        }

        let mut output_buffers = Vec::with_capacity(n_out);
        for _ in 0..n_out {
            output_buffers.push(DoubleBuffer::new(frames));
        }

        let hw_ready_event = SafeHandle::create_event()?;
        let asio_ready_event = SafeHandle::create_event()?;

        Ok(Self {
            frames,
            input_buffers,
            output_buffers,
            active_index: Arc::new(AtomicI32::new(0)),
            hw_ready_event,
            asio_ready_event,
            running: Arc::new(AtomicU8::new(0)),
        })
    }

    /// Current active buffer index (atomically loaded).
    #[inline(always)]
    pub fn current_index(&self) -> BufferIndex {
        self.active_index.load(Ordering::Acquire)
    }

    /// Swap the active buffer index (0 → 1 → 0 …).
    #[inline(always)]
    pub fn swap_index(&self) -> BufferIndex {
        let prev = self.active_index.fetch_xor(1, Ordering::AcqRel);
        prev ^ 1
    }

    /// Raw pointer to the ASIO info struct expected by the host for input channel `ch`.
    pub fn input_ptr(&self, ch: usize, index: BufferIndex) -> *mut std::ffi::c_void {
        self.input_buffers[ch].ptr(index) as *mut std::ffi::c_void
    }

    /// Raw pointer to the ASIO info struct expected by the host for output channel `ch`.
    pub fn output_ptr(&self, ch: usize, index: BufferIndex) -> *mut std::ffi::c_void {
        self.output_buffers[ch].ptr(index) as *mut std::ffi::c_void
    }

    /// Signal that the hardware I/O thread has produced new data.
    pub fn signal_hw_ready(&self) {
        unsafe { let _ = SetEvent(self.hw_ready_event.0); }
    }

    /// Signal that the ASIO host has finished processing (`outputReady`).
    pub fn signal_asio_ready(&self) {
        unsafe { let _ = SetEvent(self.asio_ready_event.0); }
    }

    /// Wait (in the KS thread) until the ASIO host is done with its buffer.
    pub fn wait_asio_ready(&self) {
        unsafe {
            let _ = ResetEvent(self.asio_ready_event.0);
            WaitForSingleObject(self.asio_ready_event.0, INFINITE);
        }
    }

    /// Wait (in the ASIO callback) until new hardware data is available.
    pub fn wait_hw_ready(&self) {
        unsafe {
            let _ = ResetEvent(self.hw_ready_event.0);
            WaitForSingleObject(self.hw_ready_event.0, INFINITE);
        }
    }

    /// Mark streaming as active.
    pub fn set_running(&self, running: bool) {
        self.running.store(running as u8, Ordering::Release);
    }

    /// Returns true while streaming is active.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire) != 0
    }
}

// ---------------------------------------------------------------------------
// SafeHandle — RAII wrapper for a Windows HANDLE
// ---------------------------------------------------------------------------

pub struct SafeHandle(pub HANDLE);

impl SafeHandle {
    pub fn create_event() -> windows::core::Result<Self> {
        let h = unsafe { CreateEventW(None, true, false, None)? };
        Ok(Self(h))
    }
}

impl Drop for SafeHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe { let _ = CloseHandle(self.0); }
        }
    }
}

unsafe impl Send for SafeHandle {}
unsafe impl Sync for SafeHandle {}
