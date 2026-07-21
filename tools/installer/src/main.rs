//! 1-Click Installer and Uninstaller for iRig USB ASIO Driver (Rust).

#![windows_subsystem = "windows"]

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, SW_SHOWNORMAL, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
};

const APP_NAME: &str = "iRig USB ASIO Driver (Rust)";
const REG_UNINSTALL_PATH: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\iRigAsioDriver";

// Embed the compiled DLL binary payload
const EMBEDDED_DLL: &[u8] = include_bytes!("../../../target/release/irig_asio.dll");

fn is_elevated() -> bool {
    // Attempt to open HKLM for write access to check admin privileges
    unsafe {
        let mut key = HKEY::default();
        let subkey: Vec<u16> = "SOFTWARE\0".encode_utf16().collect();
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            0,
            KEY_WRITE,
            &mut key,
        )
        .is_ok()
    }
}

fn relaunch_as_admin() {
    if let Ok(exe_path) = env::current_exe() {
        let exe_utf16: Vec<u16> = exe_path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        let runas_utf16: Vec<u16> = "runas\0".encode_utf16().collect();
        let args: Vec<String> = env::args().skip(1).collect();
        let args_str = args.join(" ");
        let args_utf16: Vec<u16> = args_str.encode_utf16().chain(std::iter::once(0)).collect();

        unsafe {
            ShellExecuteW(
                HWND::default(),
                PCWSTR(runas_utf16.as_ptr()),
                PCWSTR(exe_utf16.as_ptr()),
                PCWSTR(if args_str.is_empty() { std::ptr::null() } else { args_utf16.as_ptr() }),
                PCWSTR(std::ptr::null()),
                SW_SHOWNORMAL,
            );
        }
    }
}

fn show_info(msg: &str) {
    let title_utf16: Vec<u16> = format!("{APP_NAME} Setup\0").encode_utf16().collect();
    let msg_utf16: Vec<u16> = format!("{msg}\0").encode_utf16().collect();
    unsafe {
        MessageBoxW(
            HWND::default(),
            PCWSTR(msg_utf16.as_ptr()),
            PCWSTR(title_utf16.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

fn show_error(msg: &str) {
    let title_utf16: Vec<u16> = format!("{APP_NAME} Setup Error\0").encode_utf16().collect();
    let msg_utf16: Vec<u16> = format!("{msg}\0").encode_utf16().collect();
    unsafe {
        MessageBoxW(
            HWND::default(),
            PCWSTR(msg_utf16.as_ptr()),
            PCWSTR(title_utf16.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn get_install_dir() -> PathBuf {
    let program_files = env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    PathBuf::from(program_files).join("iRigAsioDriver")
}

fn install() -> Result<(), String> {
    let install_dir = get_install_dir();
    fs::create_dir_all(&install_dir)
        .map_err(|e| format!("Failed to create install directory {}: {e}", install_dir.display()))?;

    let dll_path = install_dir.join("irig_asio.dll");
    fs::write(&dll_path, EMBEDDED_DLL)
        .map_err(|e| format!("Failed to write driver DLL {}: {e}", dll_path.display()))?;

    // Copy installer executable as uninstaller
    if let Ok(current_exe) = env::current_exe() {
        let uninstaller_path = install_dir.join("uninstall.exe");
        let _ = fs::copy(&current_exe, &uninstaller_path);
    }

    // Call regsvr32 to register COM server & ASIO keys
    let status = Command::new("regsvr32.exe")
        .arg("/s")
        .arg(&dll_path)
        .status()
        .map_err(|e| format!("Failed to run regsvr32: {e}"))?;

    if !status.success() {
        return Err("regsvr32 registration failed".into());
    }

    // Register in Windows Add/Remove Programs (Uninstall key)
    register_uninstall_entry(&install_dir)?;

    show_info(&format!(
        "Installation successful!\n\n\
        The iRig USB ASIO Driver has been installed to:\n{}\n\n\
        You can now select 'iRig USB ASIO' in your DAW or ToneLib GFX.",
        dll_path.display()
    ));

    Ok(())
}

fn register_uninstall_entry(install_dir: &PathBuf) -> Result<(), String> {
    unsafe {
        let mut key = HKEY::default();
        let subkey_utf16: Vec<u16> = format!("{REG_UNINSTALL_PATH}\0").encode_utf16().collect();

        let res = RegCreateKeyW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey_utf16.as_ptr()),
            &mut key,
        );

        if res.is_err() {
            return Err("Failed to create Windows Uninstall registry key".into());
        }

        let uninstaller_exe = install_dir.join("uninstall.exe");
        let uninstall_cmd = format!("\"{}\" --uninstall", uninstaller_exe.display());

        let set_string = |name: &str, val: &str| {
            let n_utf16: Vec<u16> = format!("{name}\0").encode_utf16().collect();
            let v_utf16: Vec<u16> = format!("{val}\0").encode_utf16().collect();
            let bytes = std::slice::from_raw_parts(v_utf16.as_ptr() as *const u8, v_utf16.len() * 2);
            let _ = RegSetValueExW(key, PCWSTR(n_utf16.as_ptr()), 0, REG_SZ, Some(bytes));
        };

        set_string("DisplayName", APP_NAME);
        set_string("UninstallString", &uninstall_cmd);
        set_string("DisplayVersion", "0.1.0");
        set_string("Publisher", "Open Source / Community");
        set_string("DisplayIcon", &uninstaller_exe.to_string_lossy());

        let _ = RegCloseKey(key);
    }
    Ok(())
}

fn uninstall() -> Result<(), String> {
    let install_dir = get_install_dir();
    let dll_path = install_dir.join("irig_asio.dll");

    // Unregister COM server & ASIO keys
    if dll_path.exists() {
        let _ = Command::new("regsvr32.exe")
            .arg("/u")
            .arg("/s")
            .arg(&dll_path)
            .status();
    }

    // Delete uninstall registry key
    unsafe {
        let subkey_utf16: Vec<u16> = format!("{REG_UNINSTALL_PATH}\0").encode_utf16().collect();
        let _ = RegDeleteKeyW(HKEY_LOCAL_MACHINE, PCWSTR(subkey_utf16.as_ptr()));
    }

    // Delete files
    let _ = fs::remove_file(&dll_path);

    show_info("iRig USB ASIO Driver uninstalled successfully.");
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let is_uninstall = args.iter().any(|a| a == "--uninstall");

    if !is_elevated() {
        relaunch_as_admin();
        return;
    }

    if is_uninstall {
        if let Err(e) = uninstall() {
            show_error(&format!("Uninstall failed: {e}"));
        }
    } else {
        if let Err(e) = install() {
            show_error(&format!("Installation failed: {e}"));
        }
    }
}
