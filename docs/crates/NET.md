# `net` Crate

All networking code: UDP/TCP transports, authenticated encryption primitives, device discovery, audio bridge, file transfer, folder synchronization, and the mobile HTTPS server.

---

## Overview

| Item | Value |
|---|---|
| Crate name | `net` |
| Type | Library (`lib`) |
| Key dependencies | `tokio`, `chacha20poly1305`, `sha2`, `hmac`, `tokio-rustls`, `rcgen`, `socket2`, `serde_json` |

---

## Modules

### `secure_input` — Encryption Primitives

Core AEAD layer used by all data channels.

**Key derivation:**
```
cipher_key = SHA-256( "NerveDesk v2:" || context || pairing_key )
```
The `context` byte string differentiates channels (e.g. `b"input"`, `b"audio-packet"`, `b"folder-sync:…"`).

**Packet format:**
```
[ nonce: 12 bytes ] [ ciphertext ] [ Poly1305 tag: 16 bytes ]
```
Nonces are generated with `BCryptGenRandom` (Windows CSPRNG).

**`ReplayGuard`** maintains a sliding window of 4096 recently seen nonces. Duplicate nonces are rejected before decryption is attempted.

**`seal(key, plaintext)`** / **`ReplayGuard::open(key, packet)`** are the primary entry points used by all other modules.

---

### `udp` — Input Transport

`UdpInputSender` — encrypts and sends `InputEvent` datagrams.
`UdpInputReceiver` — receives, verifies, and decodes datagrams.

- IP filter: packets from addresses other than `paired_peer_ip` are silently discarded.
- Pairing handshake: probe (`b"pair-probe"` context) → ack (`b"pair-ack"` context) with challenge-response to confirm the peer holds the same key.

Port: **UDP 48100**.

---

### `discovery` — Device Discovery

**Wire format (signed):**
```
[ payload_len: 4 bytes LE ] [ JSON beacon ] [ HMAC-SHA256: 32 bytes ]
```
**Wire format (unsigned):** plain JSON beacon (broadcast only).

`DiscoveryBeaconEmitter`:
- `emit_beacon()` — unsigned UDP broadcast to `255.255.255.255:48103`.
- `emit_to(addr)` — HMAC-signed unicast to the paired peer.
- `set_pairing_key(key)` — updates the signing key at runtime.

`DiscoveryListener`:
- `recv_beacon(pairing_key)` — accepts unsigned beacons when `pairing_key` is `None`; requires a valid HMAC when `Some`.
- `reply_to(beacon, addr, pairing_key)` — signs the reply when talking to the paired peer.

Port: **UDP 48103**.

---

### `video` — Mirror Video Transport

`VideoSender` streams H.264 NAL units or JPEG frames over TCP.
`VideoReceiver` accepts incoming streams, verifies authentication, and forwards decoded frames to the viewer.

Ports: **TCP 48105** (H.264), **TCP 48104** (JPEG).

---

### `mirror` — IddCx Driver Stream

`MirrorSender` / `MirrorReceiver` handle the zlib-compressed BGRA stream from the IddCx virtual display driver.

Codec negotiation: `Codec::Zlib` for the driver path.

Port: **UDP 48101** (driver loopback output from the host app).

---

### `audio` — Audio Bridge

`AudioBridge` manages bidirectional WASAPI audio streaming.

- Receiver listens on **TCP 48107**. Accepts connections only from `paired_peer_ip`.
- Sender connects to the peer's port 48107.
- Control channel on **TCP 48109** accepts `PULL` (request reverse stream) and `STOP` commands.
- All frames encrypted with `seal_payload(key, b"audio-packet", frame, MAX_PACKET_BYTES)`.

---

### `file_transfer` — File Transfer

`FileTransferServer` / `FileTransferClient`.

- Magic bytes `NRV2` authenticate the session header.
- Files transferred in 64 KB chunks, each encrypted as an independent AEAD record.
- Maximum file size: 16 GB.
- Staging directory: `%TEMP%\NerveDesk\staged\`.

Port: **TCP 48106**.

---

### `folder_sync` — Folder Synchronisation

Bidirectional event-driven background sync with conflict resolution and version history.

**Session setup:**
1. Initiator sends an encrypted setup request (signed with the pairing key).
2. Receiver waits for user approval via the UI before creating the local folder.
3. A per-session nonce is exchanged; all subsequent records use it as part of the encryption context: `folder-sync:{nonce}:{direction}:{sequence}`.

**Event-Driven Sync Engine:**
- Changes detected via kernel-level `ReadDirectoryChangesW` notifications using the `notify` crate.
- Watches active pair local directories recursively and dynamically adapts to pair additions or removals.
- A 250 ms debounce interval collapses rapid file bursts into single atomic sync operations.
- Worker wakeups (`wake_worker()`) occur immediately upon local filesystem changes, pair approvals, or user sync commands via an `Arc<(Mutex<bool>, Condvar)>`.
- Fallback 15-second heartbeat ensures sync integrity if kernel events are missed.
- The peer with the lexicographically smaller hostname coordinates bidirectional scans.
- File hashes (SHA-256) determine whether a transfer is needed.
- Conflicts: both versions are kept. Replaced/deleted files go to version history.

Port: **TCP 48108**.

---

### Socket Buffer Configuration & Tuning

To prevent TCP window throttling and buffer starvation across multi-gigabit transfers and 60+ FPS video streaming, socket buffer sizes are explicitly tuned using `socket2::SockRef`:

| Subsystem | Port / Transport | Send Buffer (`SO_SNDBUF`) | Recv Buffer (`SO_RCVBUF`) | Notes |
|---|---|---|---|---|
| **Video Mirror** | TCP 48105 | 4 MB | 4 MB | Eliminates frame drops during high-motion bursts |
| **File Transfer** | TCP 48106 | 4 MB | 4 MB | Sustains maximum LAN throughput |
| **Folder Sync** | TCP 48108 | 2 MB | 2 MB | Optimized for rapid differential file streaming |
| **Audio Bridge** | TCP 48107 | 512 KB | 512 KB | Minimizes buffering latency (`TCP_NODELAY` enabled) |
| **Secure Input** | UDP 48100 | 512 KB | 512 KB | Prevents packet loss under high-frequency mouse movements |

---

### `mobile_server` — Mobile Remote HTTPS Server

`MobileServer` runs an embedded HTTP/1.1 server over TLS 1.3.

**Endpoints:**

| Method | Path | Auth required | Description |
|---|---|---|---|
| GET | `/` | No | Mobile remote web UI (HTML) |
| GET | `/api/status` | No (IP hidden) | Screen resolution + auth state |
| POST | `/api/verify` | No | PIN verification → session token |
| GET | `/screen.jpg` | Yes | Single JPEG screenshot |
| GET | `/stream` | Yes | MJPEG live stream |
| POST | `/api/input` | Yes | Inject mouse/keyboard event |

**Security:**
- 6-digit PIN, BCryptGenRandom. Five wrong attempts per IP → 5-minute block.
- Session token: 32 bytes crypto-random, 1-hour TTL.
- `/api/status` returns the local IP only to authorised sessions.

Port: **HTTPS 48110** (configurable).

---

### `mobile_tls` — TLS Certificate Management

- Generates a self-signed certificate per local IP using `rcgen`.
- Private key encrypted with Windows DPAPI (`CryptProtectData`) and persisted to `%APPDATA%\NerveDesk\`.
- On subsequent starts, the encrypted key is decrypted and reused.
- `fingerprint(ip)` returns the SHA-256 fingerprint of the certificate DER for manual verification in the UI.

---

## Security Summary

| Channel | Encryption | Auth |
|---|---|---|
| UDP input | ChaCha20-Poly1305 | AEAD tag + ReplayGuard + IP filter |
| TCP video/audio/file/sync | ChaCha20-Poly1305 | AEAD tag + IP filter |
| Discovery broadcast | None | Magic bytes only |
| Discovery unicast (paired) | HMAC-SHA256 | Pairing key |
| Mobile HTTPS | TLS 1.3 | PIN + session token |

---

## Related

- [ARCHITECTURE.md](../ARCHITECTURE.md)
- [ADR-001](../adr/ADR-001-udp-for-input.md) — UDP for input
- [ADR-002](../adr/ADR-002-chacha20-aead.md) — ChaCha20-Poly1305
- [ADR-004](../adr/ADR-004-hmac-discovery-beacons.md) — HMAC beacons
- [ADR-005](../adr/ADR-005-tls13-mobile-server.md) — TLS mobile server
