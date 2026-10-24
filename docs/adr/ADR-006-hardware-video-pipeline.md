# ADR-006: Hardware-Accelerated Video Pipeline Architecture

| Field | Value |
|---|---|
| Status | Accepted |
| Date | 2026-10-21 |
| Deciders | NerveDesk core team |

---

## Context

Streaming 1080p and 1440p displays at 60 FPS over local networks requires balancing image fidelity, encoding latency, and CPU load.

Three capture and encoding paths were evaluated:
1. **GDI BitBlt + Software Encoder (OpenH264 / libvpx)**: Compatible with all Windows versions, but limited to 20–30 FPS with high CPU load and tearing.
2. **WebRTC Pipeline**: Offers mature adaptive bitrate algorithms, but introduces substantial abstraction layers, complex SDP/ICE negotiation, and heavy external dependency overhead.
3. **DXGI Desktop Duplication + Windows Media Foundation (MFT) Hardware Encoders**: Native Windows OS pipeline providing direct access to GPU framebuffers and hardware ASIC encoders (NVIDIA NVENC, Intel QuickSync, AMD AMF).

---

## Decision

We chose **DXGI Desktop Duplication API combined with Windows Media Foundation Hardware Transforms (MFT)** using H.264 video compression over raw TCP (:48105).

- Video frames are captured directly from GPU display surfaces using `IDXGIOutputDuplication`.
- Hardware encoding is orchestrated via `codec` crate bindings to Windows Media Foundation.
- For window-specific capture, the pipeline switches dynamically to **Windows Graphics Capture (WGC)** API.
- If hardware MFT fails to initialize on virtual machines or legacy hardware, the system transparently falls back to an independent low-latency JPEG stream on port :48104.

---

## Consequences

**Positive:**
- Sub-5 millisecond glass-to-encode latency.
- CPU consumption stays below 4% even when streaming 1440p @ 60 FPS.
- Zero external runtime DLL dependencies — all APIs are native to Windows 10 and 11.

**Negative / Accepted:**
- Windows-only API family (DXGI, D3D11, MFT).
- Multi-monitor setups require selecting the active DXGI output adapter index.
