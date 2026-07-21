use std::env;
use std::path::PathBuf;

fn main() {
    // -------------------------------------------------------------------------
    // Tell Cargo to re-run this script if the ASIO SDK headers change.
    // -------------------------------------------------------------------------
    println!("cargo:rerun-if-changed=../vendor/asio_sdk/common/asio.h");
    println!("cargo:rerun-if-changed=../vendor/asio_sdk/common/iasiodrv.h");
    println!("cargo:rerun-if-changed=../vendor/asio_sdk/common/asiodrvr.h");
    println!("cargo:rerun-if-changed=../vendor/asio_sdk/common/asiosys.h");

    // All headers live in `common/` — this SDK has no separate `host/` folder.
    let sdk_common = PathBuf::from("../vendor/asio_sdk/common");

    // -------------------------------------------------------------------------
    // Check that the ASIO SDK has been downloaded.
    // -------------------------------------------------------------------------
    if !sdk_common.join("asio.h").exists() {
        println!("cargo:warning=⚠️  ASIO SDK not found at vendor/asio_sdk/common/asio.h");
        println!("cargo:warning=   Download the free Steinberg ASIO SDK from:");
        println!("cargo:warning=   https://www.steinberg.net/asiosdk");
        println!("cargo:warning=   Extract it so vendor/asio_sdk/common/asio.h exists.");
        println!("cargo:warning=   Then run `cargo build` again.");

        let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
        std::fs::write(
            out_dir.join("asio_bindings.rs"),
            b"// ASIO SDK not yet downloaded. See build.rs for instructions.\n",
        )
        .expect("Could not write stub asio_bindings.rs");
        return;
    }

    // -------------------------------------------------------------------------
    // Run bindgen over asio.h.
    //
    // The ASIO SDK headers use C++ (`class IASIO`) but the types we need for
    // our Rust driver are plain C structs and enums.  We tell bindgen to parse
    // them as C++ (necessary for the includes to work), but only emit bindings
    // for the allowlisted C-compatible types.
    // -------------------------------------------------------------------------
    let bindings = bindgen::Builder::default()
        // Entry point header
        .header(sdk_common.join("asio.h").to_string_lossy())
        // Make all SDK headers visible to clang
        .clang_arg(format!("-I{}", sdk_common.to_string_lossy()))
        // Parse as C++ (the SDK uses `class`, but we restrict what we generate)
        .clang_arg("-x")
        .clang_arg("c++")
        .clang_arg("-std=c++14")
        // Target Windows x64 MSVC ABI
        .clang_arg("--target=x86_64-pc-windows-msvc")
        // Suppress Windows-header noise
        .clang_arg("-DWIN32")
        .clang_arg("-D_WIN32")
        .clang_arg("-D_M_AMD64")
        // Only generate bindings for the ASIO C-compatible types
        .allowlist_type("ASIOSampleType")
        .allowlist_type("ASIOSampleRate")
        .allowlist_type("ASIOChannelInfo")
        .allowlist_type("ASIOBufferInfo")
        .allowlist_type("ASIOCallbacks")
        .allowlist_type("ASIOClockSource")
        .allowlist_type("ASIOInputMonitor")
        .allowlist_type("ASIODirectMonParams")
        .allowlist_type("AsioTimeCode")
        .allowlist_type("ASIOTimeInfo")
        .allowlist_type("ASIOTime")
        .allowlist_type("ASIOError")
        .allowlist_var("ASE_.*")
        .allowlist_var("kAsioSampleType.*")
        // Derive common traits on generated structs
        .derive_debug(true)
        .derive_default(true)
        .derive_copy(true)
        // Suppress warnings about C++ types we don't generate
        .opaque_type("std::.*")
        .opaque_type("IASIO")
        .opaque_type("AsioDriver")
        // Make the generated code no_std-friendly
        .use_core()
        .ctypes_prefix("::core::ffi")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("bindgen failed to generate ASIO bindings — check LLVM/clang is installed");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_dir.join("asio_bindings.rs"))
        .expect("Could not write asio_bindings.rs");

    println!("cargo:warning=✓ ASIO bindings generated successfully");
}
