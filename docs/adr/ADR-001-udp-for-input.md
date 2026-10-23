# ADR-001: UDP for Keyboard and Mouse Transport

| Field | Value |
|---|---|
| Status | Accepted |
| Date | 2026-10-20 |
| Deciders | NerveDesk core team |

---

## Context

The KVM input channel must deliver mouse deltas and keyboard scancodes to the remote machine with the lowest achievable latency. The channel operates over a trusted local network (LAN or 5 GHz Wi-Fi).

Two transport options were evaluated:

**TCP** provides reliable, ordered delivery. However, a single dropped or reordered packet causes the entire stream to stall until retransmission completes (Head-of-Line blocking). On Wi-Fi this occurs regularly, producing visible cursor hitching and keyboard input lag.

**UDP** sends datagrams independently. Dropped packets are never retransmitted. For cursor movement this is acceptable — only the most recent delta matters. An older position update that never arrives is irrelevant because the cursor has already moved further.

---

## Decision

All mouse and keyboard events are transmitted over raw UDP datagrams on port **48100**.

- Each datagram carries one `InputEvent`, a monotonic sequence number, and a microsecond timestamp.
- Every datagram is independently encrypted with ChaCha20-Poly1305 (see [ADR-002](ADR-002-chacha20-aead.md)).
- A `ReplayGuard` with a 4096-nonce sliding window prevents replayed datagrams from injecting duplicate input.
- The receiver ignores datagrams from any IP address other than the configured `paired_peer_ip`.

---

## Consequences

**Positive:**
- Sub-millisecond input delivery. No HoL blocking. Cursor and keyboard response is smooth even during background file transfers or video stream hiccups.
- Simple implementation — no connection state, no retransmit timers.

**Negative / Accepted:**
- Lost motion packets are not retransmitted. The cursor position is always correct after the next arriving packet; no ghost movements are possible.
- Packet ordering is not guaranteed. The sequence number allows the receiver to detect out-of-order delivery, but reordering is not corrected — the freshest delta is always applied immediately.
- Replay protection consumes memory proportional to the window size (4096 nonces × 12 bytes ≈ 48 KB).
