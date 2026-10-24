# Platform, Hardware & Driver Compatibility Matrix

| Field | Value |
|---|---|
| Target Operating System | Microsoft Windows (x86_64 & ARM64) |
| Minimum Windows Version | Windows 10 Version 2004 (Build 19041) |
| Recommended Windows Version | Windows 11 Version 23H2+ |

---

## 1. Windows Operating System Compatibility

| Windows Edition / Version | Build Number | KVM Input | Virtual Display (IddCx) | DXGI Desktop Duplication | Mobile Remote (HTTPS) |
|---|---|---|---|---|---|
| **Windows 11 23H2 / 24H2** | 22631+ | Supported | Supported (IddCx v1.9) | Supported | Supported |
| **Windows 11 22H2 / 21H2** | 22000+ | Supported | Supported (IddCx v1.8) | Supported | Supported |
| **Windows 10 22H2 / 21H2** | 19044+ | Supported | Supported (IddCx v1.4) | Supported | Supported |
| **Windows 10 2004** | 19041 | Supported | Supported (IddCx v1.4) | Supported | Supported |
| **Windows 10 1809 (LTSC)** | 17763 | Supported | Not Recommended | Supported | Supported |
| **Windows on ARM64** | Windows 11 ARM | Supported (Native) | Under Verification | Supported | Supported |
| **Windows 7 / 8.1** | Any | Unsupported | Unsupported | Unsupported | Unsupported |

---

## 2. GPU Hardware Acceleration & Video Codecs

The video capture engine queries available DirectX 11 / Media Foundation hardware transforms (MFT) at startup and automatically selects the highest-performance path available on the active display adapter.

| Hardware Vendor & Architecture | Video Encoder Implementation | Max Recommended Resolution | Typical Capture-to-Encode Latency |
|---|---|---|---|
| **NVIDIA GeForce / RTX** (Kepler+) | NVIDIA NVENC Hardware MFT | 3840×2160 @ 60 FPS | 1.8 – 2.9 ms |
| **Intel Core / Arc** (Skylake+) | Intel QuickSync Video (QSV) MFT | 2560×1440 @ 60 FPS | 2.2 – 3.8 ms |
| **AMD Radeon** (GCN / RDNA) | AMD Advanced Media Framework (AMF) | 2560×1440 @ 60 FPS | 2.5 – 4.1 ms |
| **Qualcomm Snapdragon X** | Qualcomm Video Processing Unit | 1920×1080 @ 60 FPS | 3.5 – 5.5 ms |
| **Virtual Machines (No GPU)** | Windows Media Foundation Software Fallback | 1920×1080 @ 30 FPS | 12.0 – 24.0 ms |

---

## 3. Network Environment Requirements

| Requirement | Specification | Notes |
|---|---|---|
| **Local Network Type** | Gigabit Ethernet (1000BASE-T) or 5 GHz Wi-Fi (Wi-Fi 5 / 6 / 6E / 7) | 2.4 GHz Wi-Fi is usable for KVM mouse/keyboard, but display casting may experience frame jitter due to co-channel interference. |
| **Subnet Routing** | Both PCs must reside in the same IP subnet (e.g. `192.168.1.0/24`) | Discovery broadcasts (:48103) do not cross layer 3 router boundaries without IGMP proxying. Manual IP pairing works across subnets. |
| **Firewall Ingress** | Ports UDP 48100, 48101, 48103; TCP 48104–48108, 48110 | Automatically configured during NSIS installation via `NerveDesk.exe --firewall-allow`. |

---

## 4. Mobile Browser Remote Compatibility

The embedded mobile remote web application (:48110) requires zero app store installations and runs in any modern HTML5/W3C compliant browser:

- **Apple iOS / iPadOS**: Safari on iOS 15.0+ (supports Touch events, Canvas rendering, and WebSockets).
- **Google Android**: Chrome 90+, Firefox Mobile, Samsung Internet.
- **Desktop Browsers**: Google Chrome, Microsoft Edge, Mozilla Firefox, Brave.
