# NerveDesk — Low Latency KVM Virtual Display

[![Rust 1.80+](https://img.shields.io/badge/Rust-1.80%2B-orange.svg?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Tauri v2](https://img.shields.io/badge/Tauri-v2-24C8DB.svg?logo=tauri&logoColor=white)](https://v2.tauri.app/)
[![DirectX 11](https://img.shields.io/badge/DirectX-11-0078D7.svg?logo=windows&logoColor=white)](https://learn.microsoft.com/en-us/windows/win32/direct3d11/direct3d-11-graphics)
[![IddCx UMDF Driver](https://img.shields.io/badge/IddCx-UMDF_v2-00599C.svg?logo=cplusplus&logoColor=white)](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/indirect-display-driver-model-overview)
[![Tokio Async](https://img.shields.io/badge/Tokio-Async_Runtime-8A2BE2.svg)](https://tokio.rs/)
[![Windows API](https://img.shields.io/badge/Windows-API_winapi-0078D7.svg?logo=windows&logoColor=white)](https://learn.microsoft.com/en-us/windows/win32/)
[![Raw UDP](https://img.shields.io/badge/Networking-Raw_UDP-00A4EF.svg)](https://en.wikipedia.org/wiki/User_Datagram_Protocol)
[![MSVC CRT Static](https://img.shields.io/badge/MSVC-Static_CRT-5C2D91.svg)](https://learn.microsoft.com/en-us/cpp/build/reference/md-mt-ld-use-run-time-library)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

---

## Overview

**NerveDesk** is a Windows KVM (Keyboard, Video, Mouse) and display streaming project built primarily in Rust. It supports mouse and keyboard sharing and has a display transport for mirroring and virtual secondary monitor projection. The display pipeline currently uses CPU-side BGRA compression and needs performance validation.

NerveDesk sends input over UDP. Mirror mode captures the primary display through DXGI Desktop Duplication or a selected window through Windows Graphics Capture, then sends H.264 frames over TCP with JPEG fallback. Extend mode uses the NerveDesk IddCx driver to add a Windows display and stream it over the local network. Audio Bridge can send system audio to another discovered NerveDesk computer.

---

## Tech Stack

### Core Engine — `crates/`
- **Rust 1.80+**: Memory-safe, zero-cost systems programming language
- **Tokio Async Runtime**: Non-blocking asynchronous event loop and task scheduling
- **Windows API (`windows` & `winapi` crates)**: Direct access to Win32 hooks (`WH_MOUSE_LL`, `WH_KEYBOARD_LL`), DWM window composition, and input injection (`SendInput`)
- **Raw UDP Sockets**: High-throughput packet streaming with custom binary serialization and zero TCP handshake delay
- **Crossbeam Channels & Parking Lot**: Lock-free synchronization and low-overhead primitives across streaming threads

### Display & Media Pipeline
- **DirectX 11 (DXGI Desktop Duplication)**: Captures an existing Windows display for mirror mode
- **Mirror video transport**: Media Foundation H.264 over TCP 48105 with JPEG fallback on TCP 48104; the driver-backed stream retains zlib/UDP 48101
- **Audio Bridge**: WASAPI loopback capture and shared-mode playback over TCP 48107. Enable it on both computers, choose a device and a direction: send this PC's audio or request the device's audio. Reverse requests use TCP 48109. Audio is uncompressed and unencrypted, so use it on a trusted local network. Playback requires the receiver to support the sender's audio mix format.
- **Devices**: The shared device menu below Home holds the peer connection, security key, host name, and computer layout. Mirror, Audio Bridge, and Folder Sync use the same discovered computer list; each module can be enabled separately.
- **Folder Sync**: Select a folder and the other computer on either PC. The receiver approves the request and creates the same absolute path if needed. Files already present when the connection is approved stay on their original computer. Files created after connection are shared automatically in both directions; a scan runs every 600 ms. Disconnect is available in the same connection card. Folder Sync uses TCP 48108 and a generated pair secret for transfers. File contents are not encrypted in transit, so use a trusted local network. Both computers need the current NerveDesk version and network access to TCP 48108.
- **NerveDesk IddCx UMDF Driver**: C++ indirect display driver source for a virtual Windows display; release requires WDK build and Microsoft-approved signing

### Desktop GUI & Presentation — `ui/`
- **Tauri v2**: Lightweight desktop shell integrating Rust backend logic with an isolated webview renderer
- **Windows DWM Immersive Dark Mode**: Native Win32 window chrome integration (`DWMWA_USE_IMMERSIVE_DARK_MODE`, `DWMWA_CAPTION_COLOR`) with custom titlebar controls
- **System Tray Integration**: Native background minimization to Windows System Tray with double-click restore and context menu controls
- **CSS Architecture**: Minimalist dark theme inspired by Antigravity Codex, featuring custom scrollbars, monospace typography, and zero third-party telemetry

---

## Core Features

- **Seamless Border Crossing**: Mouse transitions across configured screen edges (Top, Bottom, Left, Right) automatically locking local cursor and streaming raw coordinate deltas to target host.
- **Bi-Directional Input Synchronization**: Full mouse motion, multi-button states, scroll wheels, and virtual key events dispatched using low-level Win32 `SendInput`.
- **PowerToys-Inspired Pairing**: Device discovery and pairing secured through alphanumeric security keys and unique machine names without exposing raw IP addresses.
- **Dedicated Virtual Display Streaming**: Mirror the host desktop or project an additional virtual workspace to a connected client; frame rate depends on the network and current CPU-based codec.
- **Low-Overhead Background Service**: Automatic UDP listener operating in background threads with minimal CPU and memory footprints.
- **Windows application bundle**: The PC host release must include the signed NerveDesk display driver package. The laptop receiver does not install a display driver.

---

## Documentation

- [Architecture & Data Flow](docs/ARCHITECTURE.md)
- [Repository Structure](docs/STRUCTURE.md)
- [Driver build and release](docs/DRIVER.md)
- [Mirror a screen now, without the driver](docs/MIRROR.md)

---

## Building from Source

### Prerequisites
- Windows 11 (64-bit) for extended display mode with the bundled IddCx driver
- Rust 1.80+ (MSVC toolchain: `x86_64-pc-windows-msvc`)
- Visual Studio 2022 with C++ Build Tools, Windows SDK, and Windows Driver Kit for the driver

### Build Standalone Executable
To compile the standalone GUI executable with statically linked MSVC runtime:

```powershell
$env:RUSTFLAGS="-C target-feature=+crt-static"
cargo build --release -p nervedesk-ui
```

The compiled binary will be located at:
```powershell
target\release\NerveDesk.exe
```

This standalone executable does not contain the IddCx driver package. Extended
display mode requires the signed driver installer described in
[Driver build and release](docs/DRIVER.md). Mirror mode can be used without it.

### Running CLI Daemons
Run the host streaming daemon:
```powershell
cargo run -p host -- --target <CLIENT_HOST_OR_IP>:48100 --stream-video
```

Run the client receiver daemon:
```powershell
cargo run -p client -- --bind 0.0.0.0:48100 --recv-video
```

---

## Quality Assurance & Testing

Run the full workspace automated test suite:

```powershell
cargo test --workspace
```

---

## License

This project is licensed under the MIT License. See [LICENSE](LICENSE) for details.
