# Screen Mirroring

NerveDesk supports two distinct display sharing modes that serve different use cases and have different hardware requirements.

---

## Modes

### Mirror Mode (Driverless)

Replicates an existing Windows display or a specific application window on a remote machine. No display driver is required on either PC.

- **Full-screen capture:** `DXGI Desktop Duplication API` captures the entire primary monitor as a GPU texture.
- **Window capture:** `Windows Graphics Capture API` captures a selected application window, even when another window partially overlaps it. The window must remain open and not minimized.

Both sub-modes convert captured frames to NV12 and encode them with Windows Media Foundation. The encoded stream is transmitted to the client over TCP.

### Extend Mode (Driver Required)

Creates an independent secondary Windows monitor backed by the `idd-virtual-display` IddCx UMDF driver. The driver exposes a real display device to Windows Display Settings ("Extend these displays"), allowing applications to be moved to the virtual screen and run there independently.

The driver pushes raw BGRA swap-chain frames to the NerveDesk host application over a loopback TCP connection on port 48103. The application then compresses and transmits those frames to the client.

See [DRIVER.md](DRIVER.md) for build and installation instructions.

---

## Codec Selection

| Condition | Codec | Transport |
|---|---|---|
| Hardware encoder available (NVENC / AMF / QuickSync) | H.264 (Media Foundation) | TCP 48105 |
| Hardware encoder unavailable | JPEG | TCP 48104 |
| Extend mode (IddCx driver) | zlib BGRA | UDP 48101 |

The sender automatically detects encoder availability at runtime. If the client device appears in the device list but no video arrives, verify that both machines run compatible NerveDesk builds and that the relevant ports are open on the client firewall.

---

## How to Use

1. Run NerveDesk on both the source PC and the receiving PC. Connect both to the same LAN.
2. On the receiving PC, read its Security Key in Devices. On the source PC, enter the receiver's name and key and click Connect.
3. On the source PC, open **Screen Mirror**, click **Refresh**, and select the receiving PC from the device list.
4. Choose **Entire primary screen** or a specific open application window.
5. Click **Start Streaming**. Allow NerveDesk through Windows Firewall if prompted.

The receiver displays the stream full-screen. NerveDesk preserves the source aspect ratio and centers the image with black bars when screen proportions differ. Resolution changes on either side do not require restarting the stream. Native resolution up to 3840×2160 is supported; the receiver scales the image to its display.

---

## Technical Constraints

- Mirror mode shows the same desktop on both devices — it is not a separate Windows workspace.
- Window capture stops producing frames if the captured window is minimized.
- Extend mode requires installing the IddCx driver with administrator privileges on the host PC only; the receiving PC does not need the driver.
- The portable `NerveDesk.exe` does not include the driver. Installing it separately requires a Microsoft-signed driver package (Secure Boot compatible).

---

## Troubleshooting

| Symptom | Likely Cause | Fix |
|---|---|---|
| Device visible but no video | Firewall blocking TCP 48105 or 48104 | Allow inbound TCP 48105 and 48104 on the private network profile |
| H.264 stream not starting | No hardware encoder on source PC | NerveDesk falls back to JPEG automatically |
| Window capture produces black frames | Window is minimized | Keep the captured window open |
| Extend mode not available | Driver not installed | See [DRIVER.md](DRIVER.md) |
| Stream freezes after resolution change | Encoder session restart | Stop and restart the stream |

---

## Related

- [Virtual Display Driver](DRIVER.md)
- [Architecture](ARCHITECTURE.md)
- [ADR-001: UDP for input transport](adr/ADR-001-udp-for-input.md)
