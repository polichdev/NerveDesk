# `input` Crate

Handles keyboard and mouse capture on the host, edge-crossing detection, and input injection on the client.

---

## Overview

| Item | Value |
|---|---|
| Crate name | `input` |
| Type | Library (`lib`) |
| Key dependencies | `windows-rs`, `common`, `crossbeam-channel` |

---

## Components

### Low-Level Windows Hooks

Installed via `SetWindowsHookEx` in a dedicated OS thread:

| Hook | Purpose |
|---|---|
| `WH_MOUSE_LL` | Captures absolute cursor position and button/wheel events system-wide |
| `WH_KEYBOARD_LL` | Captures virtual key codes and scan codes before they reach any application |

The hook thread runs at `THREAD_PRIORITY_TIME_CRITICAL` to minimise scheduling jitter. When the cursor crosses a configured screen edge, the hook suppresses the original event (returns non-zero from the hook procedure) and forwards the delta to the network layer.

### Edge Detection (`ScreenEdge`)

```rust
pub enum ScreenEdge {
    Left,
    Right,
    Top,
    Bottom,
}
```

`set_remote_edge(edge)` configures which boundary of the local screen connects to the remote PC. When the cursor reaches that edge, the hook begins streaming relative deltas to the remote machine and clips local cursor movement.

`set_remote_edge` is called by the engine crate whenever a signed discovery beacon from the paired peer updates the peer's reported screen edge.

### `InputEmulator`

Injects received events on the client side using the native `SendInput` API.

```rust
impl InputEmulator {
    pub fn new() -> Self;
    pub fn inject(&self, event: &InputEvent) -> Result<()>;
}
```

- Mouse move events use `MOUSEEVENTF_MOVE` with relative coordinates.
- Absolute cursor position events use `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MOVE`.
- Keyboard events use virtual key codes with `KEYEVENTF_KEYUP` / `KEYEVENTF_KEYDOWN`.
- Unicode text injection (`type_text` from mobile remote) uses `KEYEVENTF_UNICODE` directly.

---

## Thread Safety

The hook is installed and pumped from a single dedicated thread. `InputEmulator` is `Send + Sync` and can be called from any Tokio task.

---

## Related

- [COMMON.md](COMMON.md) — `InputEvent` definition
- [NET.md](NET.md) — UDP transport that carries `InputEvent` packets
- [ENGINE.md](ENGINE.md) — orchestration that wires hook output to network
