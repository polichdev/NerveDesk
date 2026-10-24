# Privacy Architecture & Zero-Telemetry Policy

| Field | Value |
|---|---|
| Target System | NerveDesk Hybrid KVM & Display Streaming Suite |
| Policy Type | Zero-Telemetry, Zero-Cloud, Peer-to-Peer Guarantee |
| Status | Active & Enforced |
| Classification | Public Privacy Specification |

---

## 1. Core Privacy Commitments

NerveDesk is designed from the ground up for environments with strict confidentiality requirements (software engineering, finance, creative production, and personal computing). Because NerveDesk handles sensitive human-machine interactions—including live display capture, keystroke input, clipboard contents, and system audio—**privacy is an architectural invariant, not an optional setting**.

### The Four Tenets:
1. **Zero External Telemetry**: NerveDesk contains no analytics libraries, tracking pixels, crash-reporting SDKs, or cloud-reporting beacons.
2. **Zero Cloud Relays (100% Peer-to-Peer)**: All data transfer occurs directly between your devices over the local area network (LAN, Wi-Fi, or direct Ethernet). Data never transits a third-party server or cloud proxy.
3. **No Central Account or Identity**: There are no accounts to create, no servers to authenticate against, and no centralized databases tracking active installations.
4. **Local Cryptographic Key Sovereignty**: All pairing keys and master secrets are generated locally on your hardware and never leave your machines.

---

## 2. Data Flow & Processing Lifecycle

| Data Category | Transport & Processing | Persistence Policy | Third-Party Visibility |
|---|---|---|---|
| **Keystrokes & Mouse Events** | Captured via Win32 low-level hooks, encrypted in RAM with ChaCha20-Poly1305, streamed over raw UDP (:48100), and injected directly into the target OS via `SendInput`. | **Never saved to disk**. Keystroke buffers exist purely in volatile memory for microseconds. | **None** (zero cloud exposure). |
| **Display & Window Capture** | Screen frames are captured directly in GPU VRAM (DirectX 11 / DXGI), compressed using on-die hardware encoders (NVENC/QSV/AMF), and transmitted over local TCP (:48105). | **Never saved to disk**. Frame textures are overwritten in GPU memory buffers. | **None** (direct peer stream). |
| **Clipboard Payloads** | Text and images copied while transitioning across screen edges are encrypted and mirrored directly to the paired machine's clipboard. | **Transient memory only**. Contents persist only in the target Windows clipboard buffer. | **None**. |
| **System Audio Streams** | Audio is captured via WASAPI Loopback, compressed to PCM/Opus, and streamed over local TCP (:48107). | **Never recorded or stored**. Streamed in real-time and played through the peer audio endpoint. | **None**. |
| **Folder Synchronization** | File deltas are monitored via `ReadDirectoryChangesW` and transferred directly to the user-selected folder on the paired PC. | Stored **only in the destination directory** explicitly configured by the user. | **None**. |
| **Device Pairing Keys** | 256-bit symmetric keys entered by the user or negotiated during local pairing. | Encrypted at rest via **Windows Data Protection API (`CryptProtectData`)** tied to the local Windows user profile. | **None**. |

---

## 3. Local Network Boundary & Beacon Privacy

### 3.1 Subnet Beacons
To discover other NerveDesk instances on your local Wi-Fi or Ethernet network, NerveDesk broadcasts a UDP discovery beacon every 2,000 ms on port **48103**.
- **Scope**: Discovery packets use IPv4 subnet broadcast (`255.255.255.255` / local subnet mask). Broadcast packets are physically blocked by standard network routers from crossing into the public internet.
- **Payload**: Contains only the local computer name, local IP address (e.g. `192.168.1.x`), and active capabilities.
- **Authentication**: When pairing is enabled, beacons include an HMAC-SHA256 signature to prevent unauthorized devices on the same Wi-Fi network from spoofing connections.

---

## 4. Mobile Remote Access Isolation

The embedded mobile web remote (:48110) allows controlling the PC from a smartphone browser without installing mobile applications:
- **Local Network Only**: The server binds to local network interfaces. It does not configure UPnP port forwarding or external STUN/TURN traversal.
- **TLS 1.3 Encryption**: All communication between the phone and PC is encrypted via TLS 1.3.
- **Physical Proximity Pairing**: Connections require scanning a dynamic QR code displayed on the host screen and entering a 6-digit cryptographic PIN with rate-limiting lockouts.

---

## 5. Independent Verification & Auditing

We encourage security researchers and privacy-conscious users to independently verify NerveDesk's zero-telemetry architecture:

### 5.1 Network Traffic Audit (Wireshark)
1. Open [Wireshark](https://www.wireshark.org/) and capture packets on your active network interface.
2. Filter for non-local traffic:
   ```text
   !(ip.dst == 192.168.0.0/16 || ip.dst == 10.0.0.0/8 || ip.dst == 172.16.0.0/12 || ip.dst == 255.255.255.255) && ip.addr == <YOUR_PC_IP>
   ```
3. Run NerveDesk. You will observe **zero outbound packets** directed to external internet IP addresses.

### 5.2 Disk I/O Audit (Process Monitor)
1. Run Microsoft Sysinternals **Process Monitor** (`ProcMon.exe`).
2. Filter by Process Name: `NerveDesk.exe`.
3. Confirm that no keylogger files, telemetry caches, or audio/screen recordings are created on disk.
