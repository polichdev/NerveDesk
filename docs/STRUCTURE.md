# Repository Structure

Overview of the workspace crate architecture and directory layout:

```
nerve-desk/
├── Cargo.toml                  Workspace root configuration
├── crates/
│   ├── common/                 Binary packet structures, network protocols, serialization
│   ├── input/                  Low-level input hooks, edge crossing detection, SendInput emulation
│   ├── net/                    UDP datagram transports and subnet beacon discovery
│   ├── capture/                DXGI Desktop Duplication and virtual display manager
│   ├── codec/                  Hardware video encoder and decoder abstractions
│   ├── clipboard/              Non-blocking clipboard synchronization
│   ├── host/                   Host streaming daemon
│   └── client/                 Client receiver daemon
├── docs/
│   ├── ARCHITECTURE.md         System architecture and network pipeline specifications
│   └── STRUCTURE.md            Repository directory structure and crate layout
├── driver/
│   └── idd-virtual-display/    Microsoft IddCx Virtual Display Driver (C++ UMDF)
├── tools/
│   └── package-windows.ps1      Bundle the signed driver with the Windows installer
└── ui/
    ├── index.html              Single-page application dashboard interface
    ├── Cargo.toml              Desktop binary package configuration
    ├── tauri.conf.json         Tauri v2 application manifest and settings
    └── src/
        ├── main.rs             Tauri lifecycle and Win32 window entry point
        └── lib.rs              IPC commands, KVM pipeline, and clipboard background tasks
```
