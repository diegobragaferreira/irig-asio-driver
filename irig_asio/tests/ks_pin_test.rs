use windows::core::{Result as WinResult, GUID, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::*;

const KSCATEGORY_CAPTURE: GUID = GUID::from_values(
    0x65E8773D, 0x8F56, 0x11D0,
    [0xA3, 0xB9, 0x00, 0xA0, 0xC9, 0x22, 0x31, 0x96],
);
const KSCATEGORY_RENDER: GUID = GUID::from_values(
    0x65E8773E, 0x8F56, 0x11D0,
    [0xA3, 0xB9, 0x00, 0xA0, 0xC9, 0x22, 0x31, 0x96],
);
const KSCATEGORY_AUDIO: GUID = GUID::from_values(
    0x6994AD04, 0x93EF, 0x11D0,
    [0xA3, 0xCC, 0x00, 0xA0, 0xC9, 0x22, 0x31, 0x96],
);

fn enumerate_category(cat: &GUID, cat_name: &str) -> WinResult<()> {
    println!("\n=== Category: {} ===", cat_name);
    unsafe {
        let hdev = SetupDiGetClassDevsW(
            Some(cat),
            PCWSTR::null(),
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )?;

        let mut index = 0u32;
        loop {
            let mut interface_data = SP_DEVICE_INTERFACE_DATA {
                cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };

            if SetupDiEnumDeviceInterfaces(hdev, None, cat, index, &mut interface_data).is_err() {
                break; // No more interfaces
            }

            let mut required_size = 0u32;
            let _ = SetupDiGetDeviceInterfaceDetailW(
                hdev,
                &interface_data,
                None,
                0,
                Some(&mut required_size),
                None,
            );

            if required_size > 0 {
                let mut buffer = vec![0u8; required_size as usize];
                let detail_data = buffer.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
                (*detail_data).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;

                if SetupDiGetDeviceInterfaceDetailW(
                    hdev,
                    &interface_data,
                    Some(detail_data),
                    required_size,
                    None,
                    None,
                ).is_ok() {
                    let path_ptr = std::ptr::addr_of!((*detail_data).DevicePath) as *const u16;
                    let mut len = 0;
                    while *path_ptr.add(len) != 0 {
                        len += 1;
                    }
                    let slice = std::slice::from_raw_parts(path_ptr, len);
                    let path_str = String::from_utf16_lossy(slice);

                    if path_str.to_lowercase().contains("vid_1963") {
                        println!("  ★ iRig Match [{}]: {}", index, path_str);
                    }
                }
            }

            index += 1;
        }

        let _ = SetupDiDestroyDeviceInfoList(hdev);
    }
    Ok(())
}

#[test]
fn test_enumerate_all_categories() -> WinResult<()> {
    enumerate_category(&KSCATEGORY_AUDIO, "KSCATEGORY_AUDIO")?;
    enumerate_category(&KSCATEGORY_RENDER, "KSCATEGORY_RENDER")?;
    enumerate_category(&KSCATEGORY_CAPTURE, "KSCATEGORY_CAPTURE")?;
    Ok(())
}
