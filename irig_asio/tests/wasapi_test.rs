//! WASAPI endpoint enumeration test with friendly names for iRig USB.

use windows::core::Result;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::PropertiesSystem::*;

const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows::core::GUID::from_values(
        0xa45c254e, 0xdf1c, 0x4efd,
        [0x80, 0x20, 0x67, 0xd1, 0x46, 0xa8, 0x50, 0xe0],
    ),
    pid: 14,
};

#[test]
fn test_enumerate_wasapi_endpoints() -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;

        println!("\n=== WASAPI Capture Endpoints (All States) ===");
        let collection = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE(DEVICE_STATEMASK_ALL))?;
        let count = collection.GetCount()?;
        for i in 0..count {
            let device = collection.Item(i)?;
            let id = device.GetId()?.to_string().unwrap_or_default();

            let props: IPropertyStore = device.OpenPropertyStore(STGM_READ)?;
            let prop_val = props.GetValue(&PKEY_DEVICE_FRIENDLY_NAME)?;
            let name = prop_val.to_string();

            println!("  Input [{}]: {} ({})", i, name, id);
        }

        println!("\n=== WASAPI Render Endpoints (Outputs) ===");
        let collection = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let count = collection.GetCount()?;
        for i in 0..count {
            let device = collection.Item(i)?;
            let id = device.GetId()?.to_string().unwrap_or_default();

            let props: IPropertyStore = device.OpenPropertyStore(STGM_READ)?;
            let prop_val = props.GetValue(&PKEY_DEVICE_FRIENDLY_NAME)?;
            let name = prop_val.to_string();

            println!("  Output [{}]: {} ({})", i, name, id);
        }
    }
    Ok(())
}
