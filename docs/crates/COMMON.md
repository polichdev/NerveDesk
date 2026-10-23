# `common` Crate

Shared foundational types used across every crate in the workspace. Defines the binary packet protocol, event model, and error taxonomy.

---

## Overview

| Item | Value |
|---|---|
| Crate name | `common` |
| Type | Library (`lib`) |
| Key dependencies | `bincode`, `serde`, `thiserror` |

---

## Packet Protocol

### `InputEvent`

The primary payload type carried over the UDP input channel.

```rust
pub enum InputEvent {
    CursorPosition { x: i32, y: i32 },
    MouseMove      { dx: i32, dy: i32 },
    MouseButton    { button: MouseButton, state: ButtonState },
    MouseWheel     { delta: i16, horizontal: bool },
    Keyboard       { vk_code: u16, scan_code: u16, state: KeyState, is_extended: bool },
}
```

### `PacketHeader`

Prepended to every encoded packet.

```rust
pub struct PacketHeader {
    pub sequence: u64,   // monotonically increasing per-sender counter
    pub timestamp_us: u64, // sender-side microsecond timestamp (UNIX epoch)
}
```

### Serialization

- Format: **bincode** (compact, little-endian, fixed-width integers).
- `encode_packet(event, seq, ts, buf)` writes header + payload into a pre-allocated `[u8; MAX_PACKET_SIZE]`.
- `decode_packet(bytes)` returns `(PacketHeader, InputEvent)`.

### Constants

| Constant | Value | Purpose |
|---|---|---|
| `MAX_PACKET_SIZE` | 256 bytes | Maximum plaintext packet before encryption overhead |

---

## Error Types

```rust
pub enum NerveError {
    Io(std::io::Error),
    Serialization(String),
    Protocol(String),
    Codec(String),
}
```

`Result<T>` is a type alias for `std::result::Result<T, NerveError>`.

---

## Related

- [NET.md](NET.md) — how packets are encrypted and transmitted
- [INPUT.md](INPUT.md) — how `InputEvent` values are produced and consumed
