# Changelog

All notable changes to NerveDesk are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Added
- **Event-Driven Folder Sync Engine**: Replaced legacy 600ms polling loop with Windows kernel `ReadDirectoryChangesW` filesystem events via the `notify` crate. Includes dynamic watch directory updates, a 250ms debouncing window, and instant condition variable signaling (`wake_worker()`) on configuration changes, pair approvals, and file events.
- **Video Transport Forward Error Correction (FEC)**: Added parity chunk generation and automatic packet loss recovery in `crates/codec/src/chunker.rs`, allowing the video receiver to reconstruct missing packets without retransmission overhead.
- **Direct Zero-Copy COM Memory Pipeline**: Implemented direct Media Foundation buffer locking (`IMFMediaBuffer::Lock`) with stride-aware `bgra_to_nv12_into` conversion and reusable encoder samples/buffers across frames in DXGI capture, eliminating per-frame heap allocations.
- **`mimalloc` Memory Allocator & Buffer Pooling**: Switched global allocator to `mimalloc` across `nervedesk-ui`, `host`, and `client` binaries for reduced memory fragmentation and superior multithreaded allocation speed under high concurrency.
- **Windows Socket Buffer Tuning**: Enhanced network throughput and eliminated buffer throttling by setting optimized socket buffer sizes via `socket2` (4 MB for video & file transfers, 2 MB for folder sync, 512 KB for low-latency audio & UDP input).
- Comprehensive GitHub architectural governance: issue forms, PR templates, and contributing guide.
- Technical specifications for wire protocol (`docs/specs/PROTOCOL.md`) and concurrency architecture (`docs/CONCURRENCY.md`).
- System failure modes and recovery procedures (`docs/FAILURES_AND_RECOVERY.md`).
- Compatibility matrix across Windows versions, GPU encoders, and drivers (`docs/COMPATIBILITY.md`).
- Threat modeling specification (`docs/THREAT_MODEL.md`) detailing STRIDE analysis and trust boundaries.
- Architecture Decision Records ADR-006 through ADR-009 covering video pipeline, IddCx driver, audio loopback, and folder sync.

### Changed
- Configured default first-launch UI language to English (`en`).
- Set all active modules and services (KVM, Cast/Mirror, Audio Bridge, Mobile Remote, Folder Sync, and sub-toggles) to be disabled by default on clean installations.

### Fixed
- Fixed Win32 titlebar fallback icon appearing upon restoring main window from system tray.
- Eliminated placeholder latency in target dropdown selects (`mirrorTargetSelect`, `audioTargetSelect`, `syncPeerSelect`, `phoneAudioDeviceSelect`) on initial tab navigation.
- Resolved incremental compilation hard-linking failure warnings on Windows by tuning development profile.

---

## [1.0.0] - 2026-10-24

### Added
- **Seamless Mouse & Keyboard (KVM)**: Sub-millisecond cursor hopping across screen edges via low-level hooks (`WH_MOUSE_LL` / `WH_KEYBOARD_LL`) and raw UDP streaming.
- **IddCx Indirect Display Driver**: High-performance virtual second display support on Windows 10/11.
- **Display Casting**: DXGI Desktop Duplication and Windows Graphics Capture streaming with hardware-accelerated H.264 encoding.
- **Low-Latency Audio Bridge**: WASAPI loopback capture and bi-directional phone audio routing via Bluetooth.
- **Zero-Config Folder Sync**: Automated bidirectional differential folder synchronization between paired PCs.
- **Fast File Transfer**: High-throughput file sharing with automatic notification and destination folder routing.
- **Mobile Remote Web App**: Embedded zero-install HTTPS/TLS 1.3 remote control with QR code pairing and PIN authentication.
- **End-to-End Cryptography**: ChaCha20-Poly1305 AEAD payload encryption, DPAPI master key storage, and HMAC-SHA256 authenticated discovery beacons.
- **Tauri v2 User Interface**: Modern frameless dark mode desktop GUI with system tray companion panel.
