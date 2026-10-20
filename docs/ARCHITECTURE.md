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
  ├─ DXGI Desktop Duplication (GPU VRAM)                │
  │         │                                           │
  │  H.264 Hardware Encoder                             │
  │         │                                           │
  │         └───> [UDP Frame Stream: Port 48101] ─────> Hardware Decoder
  │                                                     │         │
  │                                                     │   Render Surface / IddCx
```

## Subsystem Details

### Input Pipeline
- Low-level Windows hooks (`WH_MOUSE_LL`, `WH_KEYBOARD_LL`) monitor cursor positions along monitor boundaries.
- When an edge crossing is triggered, local cursor movement is clipped and converted into relative coordinate deltas.
- Events are packed into compact binary datagrams and emitted over raw UDP socket to port 48100.
- The client receives the payload via an asynchronous background receiver and injects input via native Windows `SendInput`.

### Display & Video Streaming Pipeline
- Desktop frames are captured directly from GPU VRAM via DXGI Desktop Duplication.
- Frames are encoded using hardware-accelerated video codecs (NVENC, AMF, OneVPL).
- Video chunks are streamed across a dedicated UDP video socket to the target machine.
- Frames are decoded on the receiving GPU and displayed via HTML5 Canvas or directed to a virtual monitor using the Microsoft IddCx indirect display driver.
