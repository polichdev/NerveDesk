# Architecture & Data Flow

## Overview

NerveDesk separates real-time input transmission and high-bandwidth video streaming across isolated network channels to ensure sub-millisecond input responsiveness without Head-of-Line blocking.

## Data Flow Pipeline

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
  ├─ DXGI Desktop Duplication (screen mirror)           │
  ├─ Windows Graphics Capture (chosen window)           │
  │         ├── H.264 ──> [TCP 48105] ────────────────> H.264 decoder ─> viewer
  │         └── JPEG fallback ─> [TCP 48104] ─────────> JPEG decoder ─> viewer
  │
  └─ NerveDesk IddCx swap chain (extend)
            └── zlib ───> [UDP 48101] ────────────────> zlib decoder ─> viewer
```

## Subsystem Details

### Input Pipeline
- Low-level Windows hooks (`WH_MOUSE_LL`, `WH_KEYBOARD_LL`) monitor cursor positions along monitor boundaries.
- When an edge crossing is triggered, local cursor movement is clipped and converted into relative coordinate deltas.
- Events are packed into compact binary datagrams and emitted over raw UDP socket to port 48100.
- The client receives the payload via an asynchronous background receiver and injects input via native Windows `SendInput`.

### Display & Video Streaming Pipeline
- In screen mirror mode, DXGI Desktop Duplication captures the primary physical display.
- In window mirror mode, Windows Graphics Capture captures the selected window,
  even while another window overlaps it. Minimized windows may stop producing
  frames.
- In extend mode, the NerveDesk IddCx driver creates a separate Windows display.
  It consumes IddCx swap-chain frames and transfers BGRA pixels to the local
  NerveDesk application over loopback TCP on port 48103.
- Mirror mode converts BGRA frames to NV12 and encodes H.264 with Windows Media
  Foundation. It sends framed access units over TCP 48105. If the codec is
  unavailable, JPEG remains available on TCP 48104. The receiver presents
  decoded frames in a fullscreen Win32 window. The driver-backed path still
  uses zlib over UDP 48101. File transfer uses TCP 48106. Audio Bridge captures
  the default render endpoint through WASAPI loopback and sends framed PCM to a
  discovered peer over TCP 48107 for shared-mode playback. A reverse stream
  request goes to the peer over TCP 48109; the peer then starts sending audio
  back over TCP 48107.
- Folder Sync uses TCP 48108. A setup request creates a pending pair on the
  receiver. Its approval dialog must be accepted before the receiver creates
  the same absolute folder path and enables transfers. Each machine snapshots
  existing file paths at connection time and excludes them from transfers.
  New file paths enter the two-way sync automatically. Pair state and a three-way baseline
  are saved under the NerveDesk application data directory. A generated pair secret
  authenticates subsequent challenge-response transfers. The peer with the
  lexicographically smaller computer name coordinates two-way scans every 600 ms.
  Transfers verify SHA-256 hashes; conflicting edits keep both copies, and
  replaced or deleted local files are saved in version history. Folder
  contents are sent without transport encryption.
- See [Driver build and release](DRIVER.md) for signing and packaging.
