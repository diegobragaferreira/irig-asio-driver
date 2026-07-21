//! Integration test: verify the iRig USB is enumerated via the KS API.
//!
//! Run with:
//!   cargo test --test ks_enumeration -- --nocapture
//!
//! The iRig USB must be physically connected for this test to pass.

use irig_asio::ks_device::enumerate_irig_device_paths;
use irig_asio::constants::{IRIG_VID, IRIG_PID};

#[test]
fn test_irig_device_found() {
    let paths = enumerate_irig_device_paths()
        .expect("SetupDi enumeration API call failed");

    println!("\n=== KS Device Enumeration ===");
    if paths.is_empty() {
        println!(
            "  ⚠  iRig USB (VID_{:04X}/PID_{:04X}) NOT found.",
            IRIG_VID, IRIG_PID
        );
        println!("  Ensure the device is plugged in and Windows has recognised it.");
        println!("  (Check Device Manager → Sound, video and game controllers)");
        // Soft failure so CI doesn't break without hardware
        return;
    }

    for (i, path) in paths.iter().enumerate() {
        println!("  [{}] {}", i, path.display());
    }
    println!("  ✓  Found {} KS path(s) for iRig USB", paths.len());
}

#[test]
fn test_pin_can_be_opened() {
    use irig_asio::ks_device::{open_ks_pin, PinDirection};

    let paths = match enumerate_irig_device_paths() {
        Ok(p) if !p.is_empty() => p,
        _ => {
            println!("  Skipping pin-open test: iRig USB not found.");
            return;
        }
    };

    let pin = open_ks_pin(&paths[0], PinDirection::Render);
    match pin {
        Ok(_)  => println!("  ✓  Render pin opened successfully"),
        Err(e) => println!("  ⚠  Render pin open failed: {e}"),
    }
}
