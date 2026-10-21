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
  │         └── JPEG ───> [TCP 48104] ────────────────> JPEG decoder ─> viewer
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
- Mirror mode JPEG-encodes BGRA frames and sends framed images over TCP 48104.
  The receiver decodes them and presents the latest frame in a fullscreen
  Win32 window. The driver-backed path still uses zlib over UDP 48101.
  Hardware H.264 encoding and decoding remain future work.
- See [Driver build and release](DRIVER.md) for signing and packaging.
