# Virtual Display Driver

NerveDesk uses a custom IddCx UMDF v2 Indirect Display Driver to expose an independent virtual monitor to Windows. This enables the **Extend** mode — a true second display that Windows treats identically to a physical monitor.

---

## Architecture

```
Windows Display Manager (DWM)
        │
        ▼
IddCx Framework (kernel)
        │  swap-chain frames (BGRA)
        ▼
idd-virtual-display.dll  (UMDF v2, user-mode, WUDFHost.exe)
        │  raw BGRA pixels over loopback TCP
        ▼
NerveDesk Host App  (127.0.0.1:48103)
        │  zlib compressed
        ▼
UDP 48101 ──────────────────────────────► NerveDesk Client
                                                │
                                                ▼
                                         Decode & Render
```

### Key Properties

- **User-mode driver (UMDF v2):** Runs inside `WUDFHost.exe`. A crash in the driver does not produce a BSOD; Windows restarts the driver process automatically.
- **Full DWM integration:** Windows treats the virtual monitor as a real display. Applications can be moved to it, it appears in Display Settings under "Extend these displays", and it supports full 3D acceleration.
- **EDID profiles:** The driver advertises configurable resolutions (1080p, 1440p, 4K) at 60 Hz or 120 Hz.
- **Loopback transport:** Swap-chain frames are delivered to the NerveDesk application over `127.0.0.1:48103` as raw BGRA pixel buffers, keeping driver logic minimal.

---

## Build Requirements

| Requirement | Version |
|---|---|
| Visual Studio | 2022 with C++ Build Tools |
| Windows Driver Kit (WDK) | 26100+ (NuGet package `Microsoft.Windows.WDK.x64`) |
| Windows SDK | Matching the WDK version |
| Target OS | Windows 11 x64 |

### Build Steps

1. Open `driver/idd-virtual-display/` in Visual Studio.
2. Build `NerveDeskDisplay.vcxproj` in **Release | x64** configuration.
3. Output: `NerveDeskDisplay.dll`, `NerveDeskDisplay.inf`.

---

## Signing & Release

> [!CAUTION]
> Do not ship a test-signed package to end users. Test signatures are rejected by Windows when Secure Boot is enabled.

For a production release:

1. Generate a catalog file from `NerveDeskDisplay.inf` using `Inf2Cat`.
2. Submit the driver package to Microsoft through the **Hardware Dev Center** portal for attestation signing or WHQL certification.
3. Receive back a Microsoft-signed `NerveDeskDisplay.cat`.
4. Assemble `NerveDeskDisplay.inf` + `NerveDeskDisplay.dll` + `NerveDeskDisplay.cat` in one directory.
5. Run the packaging script:
   ```powershell
   tools/package-windows.ps1 -SignedDriverDirectory <path>
   ```
   The script verifies the catalog signature, checks file membership, and bundles the three files with the Tauri NSIS installer under `resources/driver/`.

---

## Installation (End User)

The Windows installer prompts: *"Will this computer send an extended desktop?"*

- **Yes (Host PC):** Creates a root-enumerated device and installs the driver with administrator privileges.
- **No (Client PC):** Skips driver installation. The client never needs the driver.

The same installation action is also available at any time through the **Install NerveDesk Driver** button inside the app. No third-party drivers or external downloads are required.

---

## Current Status

| Item | Status |
|---|---|
| Rust application | Builds and runs |
| IddCx DLL | Built locally with VS Build Tools 2022 + WDK 26100.6584 |
| INF validation | Passed `Inf2Cat` |
| Driver signing | Test-signed (development certificate only) |
| End-to-end test | Not yet completed |
| Microsoft signing | Pending |

> Windows rejects unsigned (`0x800B0100`) and untrusted development certificate (`0x800B0109`) catalogs when Code Integrity is enforced. Do not publish a release until the driver passes HLK checks and receives Microsoft-approved signing.

---

## Related

- [Architecture](ARCHITECTURE.md)
- [Screen Mirror](MIRROR.md)
