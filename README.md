# NerveDesk

NerveDesk is an ultra-low-latency hybrid of a virtual display and a seamless KVM (Keyboard, Video, Mouse) solution for Windows.

- **Zero-Copy GPU Pipeline:** Desktop capture and video compression are handled entirely on the GPU via Direct3D 11 / DXGI Desktop Duplication and hardware encoders (NVENC, AMF, OneVPL), completely bypassing CPU memory staging.
- **Native Virtual Display Driver:** Powered by Microsoft IddCx (UMDF v2), allowing Windows to treat the client machine as a real hardware monitor with full display arrangement and 3D acceleration support.
- **Isolated Multi-Channel Networking:** Dedicated raw UDP for sub-millisecond mouse/keyboard streaming (zero Head-of-Line Blocking), low-latency UDP for video streaming, and an independent asynchronous channel for clipboard data transfers.
- **Modern Lightweight Architecture:** Engineered in Rust with an ultra-lightweight Tauri v2 management UI.

## Project Structure

```
nerve-desk/
├── crates/
│   ├── common/       # Binary POD protocols, packet types, serialization
│   ├── input/        # Windows low-level hooks, edge crossing detection, SendInput emulation
│   ├── net/          # Real-time UDP network transports, subnet discovery beacons
│   ├── capture/      # DXGI Desktop Duplication capture, virtual display manager
│   ├── codec/        # Hardware video encoder/decoder abstractions, frame chunking
│   ├── clipboard/    # Non-blocking asynchronous clipboard synchronization
│   ├── host/         # Host streaming daemon
│   └── client/       # Client receiver and player daemon
├── driver/
│   └── idd-virtual-display/ # Microsoft IddCx Virtual Display Driver (C++ UMDF)
└── ui/               # Tauri v2 desktop dashboard
```

## Running the Components

### Host CLI
```powershell
cargo run -p host -- --target <CLIENT_IP>:48100 --stream-video
```

### Client CLI
```powershell
cargo run -p client -- --bind 0.0.0.0:48100 --recv-video
```
