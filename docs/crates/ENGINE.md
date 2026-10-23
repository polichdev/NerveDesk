# `engine` Crate

Tokio-based service orchestrator. Owns `AppState` and spawns all background network workers.

---

## Overview

| Item | Value |
|---|---|
| Crate name | `engine` |
| Type | Library (`lib`) |
| Key dependencies | `tokio`, `net`, `capture`, `codec`, `clipboard`, `input`, `common` |

---

## `AppState`

Central shared state, wrapped in `Arc<Mutex<…>>` fields for thread-safe access across Tokio tasks.

| Field | Type | Description |
|---|---|---|
| `paired_peer_ip` | `Arc<Mutex<Option<String>>>` | IP address of the configured paired device |
| `paired_security_key` | `Arc<Mutex<Option<String>>>` | Shared pairing key (loaded from DPAPI store) |
| `discovered_devices` | `Arc<Mutex<HashMap<String, DiscoveredDevice>>>` | All devices seen via discovery beacons |
| `device_last_seen` | `Arc<Mutex<HashMap<String, Instant>>>` | Timestamp of last beacon per device IP |
| `authenticated_peer_seen` | `Arc<Mutex<Option<Instant>>>` | Last time the pair-probe handshake succeeded |
| `screen_edge` | `Arc<Mutex<String>>` | Configured local screen edge ("Left" / "Right" / "Top" / "Bottom") |
| `peer_device` | `Arc<Mutex<Option<String>>>` | Name of the currently active paired device |
| `audio_bridge` | `Arc<AudioBridge>` | Audio bridge state and control |
| `folder_sync` | `Arc<FolderSync>` | Folder synchronisation state |

---

## Background Services (`services.rs`)

`start_background_services(state, on_file_received)` launches all workers.

| Function | Description |
|---|---|
| `spawn_discovery_emitter` | Broadcasts unsigned beacons every 1 s; sends HMAC-signed unicast to the paired peer |
| `spawn_discovery_listener` | Receives beacons; updates `discovered_devices`; replies to broadcast senders; applies peer screen-edge changes |
| `spawn_pair_probe` | Sends ChaCha20-encrypted probe packets to the paired peer every 1 s; updates `authenticated_peer_seen` on successful challenge-response |
| `spawn_input_receiver` | Binds UDP 48100; decrypts and injects `InputEvent` packets via `InputEmulator` |
| `spawn_clipboard_receiver` | Listens for inbound clipboard payloads; applies them via the clipboard crate |
| `spawn_clipboard_sender` | Monitors local clipboard changes; deduplicates and transmits to the paired peer |
| `spawn_video_receiver` | Binds TCP 48105/48104; decodes received H.264/JPEG frames; renders to the viewer window |
| `spawn_mirror_receiver` | Binds UDP 48101; receives and decompresses IddCx driver frames |

---

## Mirror Worker (`mirror_worker.rs`)

`MirrorWorker` runs the host-side screen capture + encode + transmit loop:

1. Receives BGRA frames from the `capture` crate.
2. Encodes with `HardwareVideoEncoder` (codec crate).
3. Streams NAL units to the remote client via `VideoSender` (net crate).

---

## Badge Notifications (`badge.rs`)

`show_badge(text)` updates the Tauri system-tray badge and optionally triggers a Windows toast notification to surface background status changes (e.g. new device discovered, pairing key mismatch).

---

## Viewer (`viewer.rs`)

Creates and manages a fullscreen Win32 window for displaying decoded video frames received from the remote host. Handles resolution changes without restarting the stream.

---

## Related

- [NET.md](NET.md) — all network modules used by the services
- [INPUT.md](INPUT.md) — `InputEmulator` called by `spawn_input_receiver`
- [CAPTURE.md](CAPTURE.md) — frame source for `MirrorWorker`
- [CODEC.md](CODEC.md) — encoder/decoder used by `MirrorWorker` and `spawn_video_receiver`
- [CLIPBOARD.md](CLIPBOARD.md) — clipboard workers
