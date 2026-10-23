# Repository Structure

```
nerve-desk/
├── Cargo.toml                   Workspace root
├── Cargo.lock
├── SECURITY.md                  Security policy and vulnerability reporting
│
├── crates/
│   ├── common/                  Shared packet protocol, types, and errors
│   ├── input/                   Windows hooks, edge detection, SendInput emulation
│   ├── net/                     All network transports, security primitives, mobile server
│   ├── capture/                 DXGI/WGC screen capture and IddCx loopback receiver
│   ├── codec/                   Media Foundation H.264 encoder/decoder, JPEG fallback
│   ├── clipboard/               Async clipboard synchronization (DIB, text, images)
│   ├── engine/                  Tokio service orchestrator, AppState, background workers
│   ├── host/                    Host CLI daemon entry point
│   └── client/                  Client CLI daemon entry point
│
├── docs/
│   ├── ARCHITECTURE.md          System overview, data flow, security model
│   ├── MIRROR.md                Screen mirror and extend display modes
│   ├── DRIVER.md                IddCx driver build, signing, and release
│   ├── STRUCTURE.md             This file
│   ├── crates/
│   │   ├── COMMON.md            common crate reference
│   │   ├── INPUT.md             input crate reference
│   │   ├── NET.md               net crate reference
│   │   ├── CAPTURE.md           capture crate reference
│   │   ├── CODEC.md             codec crate reference
│   │   ├── CLIPBOARD.md         clipboard crate reference
│   │   └── ENGINE.md            engine crate reference
│   └── adr/
│       ├── ADR-001-udp-for-input.md
│       ├── ADR-002-chacha20-aead.md
│       ├── ADR-003-dpapi-key-storage.md
│       ├── ADR-004-hmac-discovery-beacons.md
│       └── ADR-005-tls13-mobile-server.md
│
├── driver/
│   └── idd-virtual-display/     C++ UMDF v2 IddCx indirect display driver
│       ├── Driver.cpp / Driver.h
│       └── NerveDeskDisplay.inf
│
├── ui/
│   ├── index.html               Single-page application
│   ├── tray.html                System tray popup
│   ├── tauri.conf.json          Tauri v2 manifest
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs              Entry point
│       ├── lib.rs               Tauri setup and plugin registration
│       ├── commands.rs          IPC command handlers
│       ├── firewall.rs          Windows Firewall rule management
│       ├── secret_store.rs      Pairing key storage via Windows DPAPI
│       ├── tray.rs              System tray icon and menu
│       └── window.rs            Win32 window helpers
│
└── agent/                       Development rules, roadmap, tech stack notes
```

## Crate Dependency Graph

```
ui
└── engine
    ├── net
    │   ├── common
    │   ├── input
    │   └── codec
    ├── capture
    │   └── common
    ├── clipboard
    │   └── common
    └── common

host  ──► net, input, common
client ──► net, codec, common
```
