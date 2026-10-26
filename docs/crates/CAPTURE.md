# `capture` Crate

Screen capture from physical displays and the IddCx virtual display driver.

---

## Overview

| Item | Value |
|---|---|
| Crate name | `capture` |
| Type | Library (`lib`) |
| Key dependencies | `windows-rs` (DXGI, D3D11, WGC, WinRT), `common` |

---

## Components

### DXGI Desktop Duplication (`dxgi.rs`)

Captures the primary physical monitor using the `IDXGIOutputDuplication` API.

**Pipeline:**
1. `AcquireNextFrame` returns a `IDXGIResource` pointing to a GPU `ID3D11Texture2D` in VRAM.
2. No CPU copy at this stage — the texture remains in GPU memory.
3. The texture is shared with the codec crate via a Direct3D 11 staging texture or shared handle.
4. `ReleaseFrame` returns the surface to the OS.

**Dirty rectangles:** The API provides metadata about which regions changed since the last frame, enabling the encoder to skip unchanged areas.

### Windows Graphics Capture (`window.rs`)

Captures a specific application window using the `Windows.Graphics.Capture` WinRT API.

- Works even when another window partially overlaps the target.
- Stops producing frames when the window is minimized.
- Returns frames as `Direct3D11CaptureFrame` (GPU texture via WinRT interop).

### Virtual Display Loopback (`virtual_display.rs` / `driver.rs`)

Receives BGRA pixel frames from the `idd-virtual-display` driver over a local TCP connection on `127.0.0.1:48103`.

The driver pushes swap-chain frames as raw BGRA buffers. The capture crate accepts these frames and forwards them to the engine for encoding and transmission.

---

## Frame Format & Zero-Allocation Conversion

All capture paths produce `BGRA` pixel data (4 bytes per pixel, row-major). 

**Direct COM Memory Locking Pipeline:**
- Instead of allocating intermediate CPU vectors for color space conversion, the pipeline locks the Media Foundation input buffer directly via `IMFMediaBuffer::Lock`.
- In-place conversion is performed via `bgra_to_nv12_into`, writing directly from the mapped DXGI surface into the encoder's locked buffer in a single stride-aware pass.
- Encoder input samples (`IMFSample`) and memory buffers are cached and reused across frames, eliminating per-frame COM allocations and garbage collection overhead.

---

## Related

- [CODEC.md](CODEC.md) — consumes captured frames
- [DRIVER.md](../DRIVER.md) — produces loopback frames
- [ARCHITECTURE.md](../ARCHITECTURE.md) — full capture-to-client pipeline
