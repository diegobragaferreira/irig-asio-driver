//! Shared error types and constants for the iRig USB ASIO driver.

use windows::core::HRESULT;

// ---------------------------------------------------------------------------
// ASIO error codes (mirror of ASIOError in the SDK)
// These are used in the ASIO COM interface methods which return i32.
// ---------------------------------------------------------------------------

/// Operation completed successfully.
pub const ASE_OK: i32 = 0;
/// No input/output is present.
pub const ASE_NO_MEMORY: i32 = -1001;
/// Hardware input or output is not present or available.
pub const ASE_HW_BAD_PARAMETER: i32 = -1003;
/// Invalid parameter.
pub const ASE_INVALID_PARAMETER: i32 = -1002;
/// Hardware is malfunctioning.
pub const ASE_HW_FAILURE: i32 = -1000;
/// No clock sources available.
pub const ASE_NO_CLOCK_SOURCES: i32 = -1005;
/// Sample rate has not been set.
pub const ASE_NOT_PRESENT: i32 = -1006;

// ---------------------------------------------------------------------------
// Device constants
// ---------------------------------------------------------------------------

/// USB Vendor ID for IK Multimedia.
pub const IRIG_VID: u32 = 0x1963;

/// USB Product ID for the iRig USB.
pub const IRIG_PID: u32 = 0x00C2;

/// Human-readable device name shown in DAWs.
pub const DRIVER_NAME: &str = "iRig USB ASIO (Rust)";

/// ASIO driver version reported to host applications.
pub const DRIVER_VERSION: i32 = 1;

// ---------------------------------------------------------------------------
// Audio format defaults
// ---------------------------------------------------------------------------

/// Default (and only) sample rate we expose.
pub const DEFAULT_SAMPLE_RATE: f64 = 48_000.0;

/// Supported sample rate — 44.1 kHz is also reported.
pub const SAMPLE_RATE_44100: f64 = 44_100.0;

/// Number of input channels (stereo).
pub const NUM_INPUT_CHANNELS: i32 = 2;

/// Number of output channels (stereo).
pub const NUM_OUTPUT_CHANNELS: i32 = 2;

/// Minimum ASIO buffer size in frames.
pub const BUFFER_SIZE_MIN: i32 = 64;

/// Preferred ASIO buffer size in frames (~5 ms @ 48 kHz).
pub const BUFFER_SIZE_PREFERRED: i32 = 256;

/// Maximum ASIO buffer size in frames.
pub const BUFFER_SIZE_MAX: i32 = 1024;

/// Buffer size must be a power of 2; granularity = -1 means powers of 2 only.
pub const BUFFER_SIZE_GRANULARITY: i32 = -1;

/// Sample format reported to the host (32-bit IEEE float).
/// Corresponds to ASIOSTFloat32LSB (19) in the ASIO SDK.
pub const ASIO_SAMPLE_TYPE: i32 = 19; // ASIOSTFloat32LSB

// ---------------------------------------------------------------------------
// COM / HRESULT helpers
// ---------------------------------------------------------------------------

/// `S_OK`
pub const S_OK: HRESULT = HRESULT(0);
/// `S_FALSE`
pub const S_FALSE: HRESULT = HRESULT(1);
/// `E_NOTIMPL`
pub const E_NOTIMPL: HRESULT = HRESULT(0x80004001_u32 as i32);
/// `E_UNEXPECTED`
pub const E_UNEXPECTED: HRESULT = HRESULT(0x8000FFFF_u32 as i32);
/// `E_OUTOFMEMORY`
pub const E_OUTOFMEMORY: HRESULT = HRESULT(0x8007000E_u32 as i32);
/// `CLASS_E_NOAGGREGATION`
pub const CLASS_E_NOAGGREGATION: HRESULT = HRESULT(0x80040110_u32 as i32);
/// `E_NOINTERFACE`
pub const E_NOINTERFACE: HRESULT = HRESULT(0x80004002_u32 as i32);

// ---------------------------------------------------------------------------
// Registry paths
// ---------------------------------------------------------------------------

/// Root key for all ASIO driver registrations.
pub const ASIO_REGISTRY_ROOT: &str = r"SOFTWARE\ASIO";

/// Subkey name for this specific driver.
pub const ASIO_REGISTRY_SUBKEY: &str = r"SOFTWARE\ASIO\iRig USB ASIO (Rust)";

/// COM CLSID for the iRig ASIO driver.
/// Generated once; do NOT change — changing it breaks existing DAW project files.
pub const DRIVER_CLSID_STR: &str = "{8F3D4A2B-E1C7-4F89-A0D3-6B2E9C1F5847}";
