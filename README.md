# NerveDesk — Ultra-Low Latency Hybrid KVM & Virtual Display for Windows

[![Rust 1.80+](https://img.shields.io/badge/Rust-1.80%2B-orange.svg?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Tauri v2](https://img.shields.io/badge/Tauri-v2-24C8DB.svg?logo=tauri&logoColor=white)](https://v2.tauri.app/)
[![DirectX 11](https://img.shields.io/badge/DirectX-11-0078D7.svg?logo=windows&logoColor=white)](https://learn.microsoft.com/en-us/windows/win32/direct3d11/direct3d-11-graphics)
[![IddCx UMDF Driver](https://img.shields.io/badge/IddCx-UMDF_v2-00599C.svg?logo=cplusplus&logoColor=white)](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/indirect-display-driver-model-overview)
[![Tokio Async](https://img.shields.io/badge/Tokio-Async_Runtime-8A2BE2.svg)](https://tokio.rs/)
[![Windows API](https://img.shields.io/badge/Windows-API_winapi-0078D7.svg?logo=windows&logoColor=white)](https://learn.microsoft.com/en-us/windows/win32/)
[![Raw UDP](https://img.shields.io/badge/Networking-Raw_UDP-00A4EF.svg)](https://en.wikipedia.org/wiki/User_Datagram_Protocol)
[![H.264 Codec](https://img.shields.io/badge/Video-H.264_Hardware-FF0000.svg)](https://en.wikipedia.org/wiki/Advanced_Video_Coding)
[![MSVC CRT Static](https://img.shields.io/badge/MSVC-Static_CRT-5C2D91.svg)](https://learn.microsoft.com/en-us/cpp/build/reference/md-mt-ld-use-run-time-library)
[![License: MIT or Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)

---

## Overview

**NerveDesk** is a high-performance, ultra-low-latency hybrid KVM (Keyboard, Video, Mouse) and virtual display streaming platform engineered natively for Windows in Rust. It enables seamless cross-device mouse and keyboard transitions across physical boundary edges with sub-millisecond input response, alongside high-frame-rate hardware-accelerated desktop mirroring and virtual secondary monitor projection.

NerveDesk eliminates Head-of-Line blocking through decoupled binary datagram protocols over raw UDP, captures desktop frames directly within GPU memory via DXGI Desktop Duplication, and links directly into the Windows display subsystem via a dedicated Microsoft Indirect Display Driver (IddCx).

---

## Tech Stack

### Core Engine — `crates/`
- **Rust 1.80+**: Memory-safe, zero-cost systems programming language
- **Tokio Async Runtime**: Non-blocking asynchronous event loop and task scheduling
- **Windows API (`windows` & `winapi` crates)**: Direct access to Win32 hooks (`WH_MOUSE_LL`, `WH_KEYBOARD_LL`), DWM window composition, and input injection (`SendInput`)
- **Raw UDP Sockets**: High-throughput packet streaming with custom binary serialization and zero TCP handshake delay
- **Crossbeam Channels & Parking Lot**: Lock-free synchronization and low-overhead primitives across streaming threads

### Display & Media Pipeline
- **DirectX 11 (DXGI Desktop Duplication)**: Zero-copy GPU desktop frame grabbing bypassing system RAM staging
- **Hardware Video Acceleration**: Pipeline abstractions for NVENC, AMD AMF, and Intel OneVPL H.264 streaming
- **Microsoft IddCx UMDF v2 Driver**: C++ indirect display driver creating virtual hardware displays recognized natively by Windows Display Settings

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
- **Dedicated Virtual Display Streaming**: Mirror the host desktop or project an additional virtual workspace to a connected client at 60+ FPS.
- **Low-Overhead Background Service**: Automatic UDP listener operating in background threads with minimal CPU and memory footprints.
- **Standalone Single-File Executable**: Built with statically linked MSVC runtime libraries (`+crt-static`), requiring zero external dependencies or runtimes.

---

## Architecture & Data Flow

```
[ Host Machine ]                                  [ Client Machine ]
  │                                                     │
  ├─ WH_MOUSE_LL / WH_KEYBOARD_LL                       │
  │  (Low-Level Input Hooks)                            │
  │         │                                           │
  │  Edge Detection ──> [UDP Datagram: Port 48100] ───> UdpInputReceiver
  │  (Border Crossing)                                  │
  │                                                     ├─ Windows SendInput
  │                                                     │  (Synthesize Motion/Keys)
  │                                                     │
  ├─ DXGI Desktop Duplication (GPU VRAM)                │
  │         │                                           │
  │  H.264 Hardware Encoder                             │
  │         │                                           │
  │         └───> [UDP Frame Stream: Port 48101] ─────> Hardware Decoder
  │                                                     │         │
  │                                                     │   Render Surface / IddCx
```

---

## Repository Structure

```
nerve-desk/
├── Cargo.toml                  Workspace root definition
├── crates/
│   ├── common/                 Binary packet structures, network protocols, serialization
│   ├── input/                  Low-level input hooks, edge crossing detection, SendInput emulation
│   ├── net/                    UDP datagram transports and subnet beacon discovery
│   ├── capture/                DXGI Desktop Duplication and virtual display manager
│   ├── codec/                  Hardware video encoder and decoder abstractions
│   ├── clipboard/              Non-blocking clipboard synchronization
│   ├── host/                   Host streaming daemon
│   └── client/                 Client receiver daemon
├── driver/
│   └── idd-virtual-display/    Microsoft IddCx Virtual Display Driver (C++ UMDF)
└── ui/
    ├── index.html              Single-page application dashboard interface
    ├── src-tauri/
    │   ├── Cargo.toml          Desktop binary configuration (NerveDesk.exe)
    │   ├── src/
    │   │   ├── main.rs         Tauri application lifecycle, tray icon, Win32 window hooks
    │   │   └── lib.rs          IPC commands, state management, background receiver tasks
    │   └── tauri.conf.json     Tauri v2 manifest, bundle settings, and security policies
```

---

## Building from Source

### Prerequisites
- Windows 10 / 11 (64-bit)
- Rust 1.80+ (MSVC toolchain: `x86_64-pc-windows-msvc`)
- Visual Studio 2022 with C++ Build Tools and Windows SDK

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

This project is licensed under either of:
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
