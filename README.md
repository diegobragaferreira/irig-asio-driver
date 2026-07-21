# iRig USB ASIO Driver (Rust)

[![Download Latest Release](https://img.shields.io/github/v/release/diegobragaferreira/irig-asio-driver?label=Download%20Installer%20.exe&style=for-the-badge&color=2ea44f&logo=windows)](https://github.com/diegobragaferreira/irig-asio-driver/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.style=for-the-badge)](LICENSE)
[![Build Status](https://img.shields.io/github/actions/workflow/status/diegobragaferreira/irig-asio-driver/build.yml?style=for-the-badge)](https://github.com/diegobragaferreira/irig-asio-driver/actions)

A ultra-low latency, open-source **Windows 10/11 ASIO driver** for the **IK Multimedia iRig USB** (and other `VID_1963` audio interfaces), written 100% in **Rust**.

Directly reads 24-bit 48kHz PCM audio via Windows Kernel Streaming (`usbaudio.sys`), bypassing the Windows audio service mixer for pristine guitar tone, high fidelity, and zero-lag performance in your favorite DAWs (ToneLib GFX, Reaper, Ableton, FL Studio, TypePedal, etc.).

---

## 1-Click Installation

1. Download **`irig_asio_installer.exe`** from the [Latest Release](https://github.com/diegobragaferreira/irig-asio-driver/releases/latest).
2. Double-click **`irig_asio_installer.exe`** to install.
3. Open your DAW or amp simulator and select **`iRig USB ASIO`** as your ASIO audio device!

*To uninstall at any time, go to Windows **Settings → Apps → Add or Remove Programs** and click **Uninstall** on "iRig USB ASIO Driver".*

---

## Features & Compatibility

- **24-bit / 16-bit 48kHz PCM Audio**: Negotiates 24-bit high-definition ADC capture directly with the hardware controller.
- **True ASIO Double-Buffering**: Lock-free ping-pong buffer management with zero audio tearing or phase artifacts.
- **Universal Hardware Compatibility**: Works with **iRig USB** (`VID_1963&PID_00C2`) and other IK Multimedia `VID_1963` class-compliant interfaces.
- **Bypasses Windows Audio Mixer**: Uses direct Kernel Streaming (`IOCTL_KS_READ_STREAM`) to eliminate OS audio degradation and latency.
- **Dynamic Buffer Sizes**: Fully supports buffer switching (64, 128, 256, 512, 1024 frames) directly from your DAW menu.

---

## Architecture

```
DAW (ToneLib GFX, Reaper, TypePedal…)
  │  ASIO COM interface (IASIO)
  ▼
irig_asio.dll          ← (User-mode Rust ASIO driver)
  │  Windows Kernel Streaming (WDM/KS)
  ▼
usbaudio.sys           ← Windows in-box UAC driver (kernel)
  │  USB isochronous
  ▼
iRig USB hardware
```

No custom kernel-mode drivers required — Windows loads `usbaudio.sys` automatically for USB Audio Class compliance.

---

## Building from Source

### Prerequisites
- Rust stable (`x86_64-pc-windows-msvc`)
- Visual Studio 2022 Build Tools (C++ workload)
- Steinberg ASIO SDK 2.3+ (extracted to `vendor/asio_sdk/`)

### Build Workspace & Installer

```powershell
# Build release workspace
cargo build --workspace --release
```

The compiled binaries will be generated at:
- `target/release/irig_asio_installer.exe` (1-click installer & uninstaller)
- `target/release/irig_asio.dll` (ASIO COM driver DLL)

---

## License

This project is licensed under the **MIT License**. Free to use, modify, and distribute for personal and commercial applications.
The Steinberg ASIO SDK is subject to Steinberg's own license — see `vendor/asio_sdk/LICENSE.txt`.
