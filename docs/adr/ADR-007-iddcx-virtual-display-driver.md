# ADR-007: Indirect Display Driver (IddCx) for Virtual Monitors

| Field | Value |
|---|---|
| Status | Accepted |
| Date | 2026-10-22 |
| Deciders | NerveDesk core team |

---

## Context

To turn a secondary laptop or tablet into a true extended Windows monitor (not merely mirroring the primary screen), the operating system must recognize a physical or virtual display adapter that exposes standard EDID timings, resolutions (1080p, 1440p, 4K), and DPI scaling.

Three approaches exist on Windows:
1. **Hardware Dummy HDMI/DisplayPort Plugs**: Works reliably but requires physical hardware dongles plugged into the PC.
2. **Mirror Driver (Legacy XDDM / WDDM Hook)**: Deprecated since Windows 8; causes Desktop Window Manager (DWM) composition failure and breaks hardware acceleration.
3. **Indirect Display Driver (IddCx / UMDF 2.x)**: Microsoft's official architecture for USB and network displays introduced in Windows 10 (version 1607+).

---

## Decision

We chose to implement a native **User-Mode Driver Framework (UMDF 2.x) Indirect Display Driver (`IddCx`)** located in `driver/idd-virtual-display/`.

- The driver exposes a virtual display device to the Windows OS Display Manager.
- When enabled via `commands::toggle_virtual_display`, Windows registers a genuine secondary desktop surface that supports standard Windows Display Settings (Arrangement, Resolution, HDR, DPI scaling).
- DWM renders desktop surfaces into driver-allocated DirectX buffers, which are captured and piped into the NerveDesk streaming engine.

---

## Consequences

**Positive:**
- Runs in User Mode (UMDF 2.x) — a driver crash cannot trigger a Windows Blue Screen of Death (BSOD).
- Full compatibility with Windows 10 and Windows 11 desktop composition and hardware-accelerated apps (DirectX, Vulkan, Chrome, Office).
- Dynamic creation and destruction of virtual screens on demand without rebooting.

**Negative / Accepted:**
- Requires Windows Developer Mode or test-signing certificate during development until WHQL attestation signing is configured.
- Requires elevated Administrator privileges for driver install/uninstall (`devcon` / `SetupAPI`).
