use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

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

fn get_config_path() -> PathBuf {
    let mut p = dirs_home().unwrap_or_else(|| PathBuf::from("."));
    p.push(".irig_asio_config.json");
    p
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(PathBuf::from)
}

struct StreamingInput {
    index: usize,
    name: String,
    capture_client: IAudioCaptureClient,
    audio_client: IAudioClient,
    max_rms: f32,
}

fn main() -> WinResult<()> {
    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║   iRig USB PCM Audio Signal & Hardware Probe v2.0       ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();
    println!("▶ Scanning & Starting Active Stream Capture on All Inputs...");

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;

        let mut streams: Vec<StreamingInput> = Vec::new();

        if let Ok(collection) = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) {
            if let Ok(count) = collection.GetCount() {
                for i in 0..count {
                    if let Ok(dev) = collection.Item(i) {
                        let name = if let Ok(props) = dev.OpenPropertyStore(STGM_READ) {
                            props.GetValue(&PKEY_DEVICE_FRIENDLY_NAME)
                                .map(|v| v.to_string())
                                .unwrap_or_else(|_| format!("Input Device #{}", i))
                        } else {
                            format!("Input Device #{}", i)
                        };

                        if let Ok(client) = dev.Activate::<IAudioClient>(CLSCTX_ALL, None) {
                            if let Ok(wfx_ptr) = client.GetMixFormat() {
                                let init_res = client.Initialize(
                                    AUDCLNT_SHAREMODE_SHARED,
                                    0,
                                    100_000,
                                    0,
                                    wfx_ptr,
                                    None,
                                );

                                if init_res.is_ok() {
                                    if let Ok(capture_client) = client.GetService::<IAudioCaptureClient>() {
                                        let _ = client.Start();
                                        streams.push(StreamingInput {
                                            index: streams.len(),
                                            name,
                                            capture_client,
                                            audio_client: client,
                                            max_rms: 0.0,
                                        });
                                    }
                                }
                                CoTaskMemFree(Some(wfx_ptr as _));
                            }
                        }
                    }
                }
            }
        }

        println!("Started active audio capture on {} physical input interfaces.", streams.len());
        println!("*** PLEASE STRUM YOUR GUITAR CONTINUOUSLY NOW (5 SECONDS) ***\n");

        let start_time = Instant::now();

        while start_time.elapsed() < Duration::from_secs(5) {
            // Read PCM buffers from all active streams
            for stream in streams.iter_mut() {
                let mut data_ptr: *mut u8 = std::ptr::null_mut();
                let mut frames_available = 0u32;
                let mut flags = 0u32;

                while let Ok(_) = stream.capture_client.GetBuffer(
                    &mut data_ptr,
                    &mut frames_available,
                    &mut flags,
                    None,
                    None,
                ) {
                    if frames_available > 0 && !data_ptr.is_null() && (flags & 1 == 0) {
                        // Calculate RMS on float/int PCM samples
                        let samples = std::slice::from_raw_parts(data_ptr as *const f32, (frames_available * 2) as usize);
                        let sum_sq: f32 = samples.iter().map(|&s| s * s).sum();
                        let rms = (sum_sq / (samples.len() as f32).max(1.0)).sqrt();

                        if rms > stream.max_rms {
                            stream.max_rms = rms;
                        }
                    }
                    let _ = stream.capture_client.ReleaseBuffer(frames_available);
                }
            }

            // Print live RMS level table
            print!("\x1B[H\x1B[J"); // Clear screen
            println!("=== LIVE PCM GUITAR SIGNAL PROBE ===");
            println!("Elapsed: {:.1}s / 5.0s\n", start_time.elapsed().as_secs_f32());

            for stream in &streams {
                let bar_len = ((stream.max_rms * 100.0) as usize).min(25);
                let bar = format!("{}{}", "█".repeat(bar_len), "░".repeat(25 - bar_len));
                let status = if stream.max_rms > 0.001 { "  ★ ACTIVE GUITAR SIGNAL DETECTED!" } else { "" };
                println!("{:2}. {:<50} [{}] RMS: {:.4}{}", stream.index, stream.name, bar, stream.max_rms, status);
            }

            thread::sleep(Duration::from_millis(50));
        }

        // Stop streams
        for stream in &streams {
            let _ = stream.audio_client.Stop();
        }

        println!("\n════════════════════════════════════════════════════════════");
        let best_stream = streams.iter().max_by(|a, b| a.max_rms.partial_cmp(&b.max_rms).unwrap());

        if let Some(best) = best_stream {
            if best.max_rms > 0.001 {
                println!("★ WINNER GUITAR INPUT DETECTED: \"{}\" (RMS Signal: {:.4})", best.name, best.max_rms);
                let config_json = format!("{{\n  \"preferred_input_device\": \"{}\"\n}}\n", best.name);
                let cfg_path = get_config_path();
                if fs::write(&cfg_path, config_json).is_ok() {
                    println!("✓ Saved preferred input device configuration to: {:?}", cfg_path);
                }
            } else {
                println!("⚠ No PCM guitar signal detected above noise floor (> 0.001).");
            }
        }
    }

    Ok(())
}
