# `codec` Crate

Hardware-accelerated H.264 video encoding and decoding via Windows Media Foundation, with a software JPEG fallback.

---

## Overview

| Item | Value |
|---|---|
| Crate name | `codec` |
| Type | Library (`lib`) |
| Key dependencies | `windows-rs` (Media Foundation, MFT), `jpeg-encoder`, `jpeg-decoder`, `common` |

---

## Encoder (`encoder.rs`)

`HardwareVideoEncoder` wraps a Windows Media Foundation transform (`IMFTransform`) for H.264 encoding.

**Hardware back-ends (selected automatically at runtime):**

| Vendor | Encoder |
|---|---|
| NVIDIA | NVENC via MFT |
| AMD | AMF via MFT |
| Intel | QuickSync via MFT |
| Fallback | Software encoder (high CPU cost) |

**Low-latency tuning:**
- Rate control: Constant Bitrate (CBR) with a 1-frame VBV buffer.
- Zero B-frames.
- Intra-refresh instead of periodic IDR frames to reduce keyframe overhead.
- `MF_LOW_LATENCY` attribute set on the MFT.

**Input:** `BGRA` or `NV12` `ID3D11Texture2D` surfaces.
**Output:** H.264 NAL unit access units, framed for TCP transmission.

---

## Decoder (`decoder.rs`)

`HardwareVideoDecoder` wraps an MFT decoder with DXVA2 / D3D11VA hardware acceleration.

- Outputs decoded frames as `NV12` textures in GPU memory.
- Frames are presented via a `IDXGISwapChain1` with `DXGI_SWAP_EFFECT_FLIP_DISCARD`.

```rust
pub struct VideoDecoderConfig {
    pub width: u32,
    pub height: u32,
    pub hwnd: HWND,   // render target window
}
```

---

## Chunker (`chunker.rs`)

Splits H.264 access unit streams into length-prefixed frames for TCP framing and reassembles them on the receiver side.

Frame format:
```
[ length: 4 bytes BE ] [ NAL unit data ]
```

---

## JPEG Fallback (`h264.rs`)

When hardware H.264 encoding is unavailable, the `jpeg-encoder` crate produces JPEG frames at configurable quality (default 80). The receiver uses `jpeg-decoder` to decompress.

JPEG frames are sent over **TCP 48104**; H.264 frames over **TCP 48105**. The sender signals which codec is active in the session handshake.

---

## Related

- [CAPTURE.md](CAPTURE.md) — provides input frames
- [NET.md](NET.md) — transports encoded output
- [MIRROR.md](../MIRROR.md) — codec selection logic
