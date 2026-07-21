//! COM registry manipulation for the iRig ASIO driver.

use windows::core::{w, Result as WinResult, PCWSTR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW,
    RegSetValueExW, HKEY, HKEY_CLASSES_ROOT, HKEY_LOCAL_MACHINE,
    KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
    REG_CREATE_KEY_DISPOSITION,
};

use crate::constants::{ASIO_REGISTRY_SUBKEY, DRIVER_CLSID_STR, DRIVER_NAME};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write a REG_SZ value to an open registry key.
unsafe fn reg_set_string(key: HKEY, value_name: PCWSTR, data: &str) -> WinResult<()> {
    let wide: Vec<u16> = data.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes: &[u8] = std::slice::from_raw_parts(
        wide.as_ptr() as *const u8,
        wide.len() * 2,
    );
    RegSetValueExW(key, value_name, 0, REG_SZ, Some(bytes)).ok()
}

/// Create (or open) a registry key and return its handle.
unsafe fn reg_create(parent: HKEY, subkey: &str) -> WinResult<HKEY> {
    let wide: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let mut hkey = HKEY::default();
    let mut disposition = REG_CREATE_KEY_DISPOSITION::default();
    RegCreateKeyExW(
        parent,
        PCWSTR(wide.as_ptr()),
        0,
        None,
        REG_OPTION_NON_VOLATILE,
        KEY_WRITE,
        None,
        &mut hkey,
        Some(&mut disposition),
    ).ok()?;
    Ok(hkey)
}

/// Delete a registry tree (key + all subkeys).
unsafe fn reg_delete_tree(parent: HKEY, subkey: &str) {
    let wide: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let _ = RegDeleteTreeW(parent, PCWSTR(wide.as_ptr()));
}

// ---------------------------------------------------------------------------
// Public registration functions
// ---------------------------------------------------------------------------

/// Register the driver in the ASIO and COM registry.
pub unsafe fn register_driver(dll_path: &str) -> WinResult<()> {
    // ── 1. ASIO enumeration key ─────────────────────────────────────────
    let asio_key = reg_create(HKEY_LOCAL_MACHINE, ASIO_REGISTRY_SUBKEY)?;
    reg_set_string(asio_key, w!("CLSID"), DRIVER_CLSID_STR)?;
    reg_set_string(asio_key, w!("Description"), DRIVER_NAME)?;
    let _ = RegCloseKey(asio_key);

    // ── 2. COM CLSID key ────────────────────────────────────────────────
    let clsid_path = format!(r"CLSID\{}", DRIVER_CLSID_STR);
    let clsid_key = reg_create(HKEY_CLASSES_ROOT, &clsid_path)?;
    reg_set_string(clsid_key, w!(""), DRIVER_NAME)?;
    let _ = RegCloseKey(clsid_key);

    // ── 3. InprocServer32 key ────────────────────────────────────────────
    let inproc_path = format!(r"CLSID\{}\InprocServer32", DRIVER_CLSID_STR);
    let inproc_key = reg_create(HKEY_CLASSES_ROOT, &inproc_path)?;
    reg_set_string(inproc_key, w!(""), dll_path)?;
    reg_set_string(inproc_key, w!("ThreadingModel"), "Apartment")?;
    let _ = RegCloseKey(inproc_key);

    log::info!("[Registration] Driver registered: CLSID {}", DRIVER_CLSID_STR);
    log::info!("[Registration]   DLL path: {}", dll_path);
    Ok(())
}

/// Remove all registry entries added by `register_driver`.
pub unsafe fn unregister_driver() -> WinResult<()> {
    reg_delete_tree(HKEY_LOCAL_MACHINE, ASIO_REGISTRY_SUBKEY);
    let clsid_path = format!(r"CLSID\{}", DRIVER_CLSID_STR);
    reg_delete_tree(HKEY_CLASSES_ROOT, &clsid_path);
    log::info!("[Registration] Driver unregistered");
    Ok(())
}

/// Retrieve the full path of this DLL using its HMODULE.
pub fn get_dll_path(module: windows::Win32::Foundation::HMODULE) -> String {
    use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
    let mut buf = [0u16; 512];
    let len = unsafe { GetModuleFileNameW(module, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..len])
}
