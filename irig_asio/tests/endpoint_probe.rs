use windows::core::{Result as WinResult, GUID};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::PropertiesSystem::*;

const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_values(
        0xa45c254e, 0xdf1c, 0x4efd,
        [0x80, 0x20, 0x67, 0xd1, 0x46, 0xa8, 0x50, 0xe0],
    ),
    pid: 14,
};

#[test]
fn test_render_and_loopback_irig() -> WinResult<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;

        println!("\n=== TESTING iRIG RENDER & LOOPBACK CAPTURE ===");
        let collection = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let count = collection.GetCount()?;

        for i in 0..count {
            let dev = collection.Item(i)?;
            let name = if let Ok(props) = dev.OpenPropertyStore(STGM_READ) {
                props.GetValue(&PKEY_DEVICE_FRIENDLY_NAME)
                    .map(|v| v.to_string())
                    .unwrap_or_default()
            } else {
                "Unknown".into()
            };

            if name.to_lowercase().contains("irig") {
                println!("\nFound Active iRig Render Endpoint [{}]: \"{}\"", i, name);

                let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
                println!("  ✓ Activate IAudioClient SUCCESS!");

                if let Ok(wfx_ptr) = client.GetMixFormat() {
                    let wfx = *wfx_ptr;
                    let rate = wfx.nSamplesPerSec;
                    let bits = wfx.wBitsPerSample;
                    let chs = wfx.nChannels;
                    println!("    Render Format: {} Hz, {} bits, {} channels", rate, bits, chs);

                    // Test LOOPBACK capture on the iRig endpoint
                    let loopback_client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
                    let loop_init = loopback_client.Initialize(
                        AUDCLNT_SHAREMODE_SHARED,
                        AUDCLNT_STREAMFLAGS_LOOPBACK,
                        100_000,
                        0,
                        wfx_ptr,
                        None,
                    );
                    println!("    Loopback Capture Initialize result: {:?}", loop_init);

                    CoTaskMemFree(Some(wfx_ptr as _));
                }
            }
        }
    }
    Ok(())
}
