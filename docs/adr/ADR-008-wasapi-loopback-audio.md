# ADR-008: WASAPI Loopback Audio Capture & Transport

| Field | Value |
|---|---|
| Status | Accepted |
| Date | 2026-10-23 |
| Deciders | NerveDesk core team |

---

## Context

Streaming game and desktop audio alongside the virtual display or KVM session requires capturing Windows system audio with minimal buffer latency and zero distortion.

Two capture approaches were evaluated:
1. **Virtual Audio Cable / Kernel Audio Filter Driver**: Intercepts the audio rendering graph by installing a virtual soundcard device. Complex to maintain, prone to driver signature issues, and can interfere with user default audio output settings.
2. **Windows Audio Session API (WASAPI) Loopback Capture**: A built-in Windows client audio API that allows capturing the exact mix sent to the default audio rendering endpoint without installing any third-party drivers.

---

## Decision

We chose **WASAPI Loopback Capture** in shared mode operating at 48,000 Hz, 16-bit stereo PCM.

- Capture operates on a dedicated thread scheduled with Multimedia Class Scheduler Service (**MMCSS**) priority (`"Audio"` class) to prevent dropouts during high CPU load.
- Captured PCM packets are encrypted with ChaCha20-Poly1305 and streamed over TCP port **48107**.
- For smartphone audio routing, the system leverages Windows Bluetooth Hands-Free / A2DP audio profiles (`bluetooth_audio` module) allowing phone-to-PC and PC-to-phone sound transfer.

---

## Consequences

**Positive:**
- Zero driver installation required for system sound streaming.
- Exact bit-accurate capture of what the user hears on their speakers or headphones.
- MMCSS integration guarantees latency under 10 milliseconds without audio stuttering.

**Negative / Accepted:**
- Captures the entire system mix (cannot isolate a single application's sound on legacy Windows 10 versions prior to Windows 10 2004 Process Loopback).
- If no application is playing audio, WASAPI loopback pauses generating packets; the receiver must handle silent gaps without desynchronizing.
