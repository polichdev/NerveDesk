# `clipboard` Crate

Asynchronous, non-blocking clipboard synchronization between the host and client.

---

## Overview

| Item | Value |
|---|---|
| Crate name | `clipboard` |
| Type | Library (`lib`) |
| Key dependencies | `windows-rs` (Win32 clipboard API), `sha2`, `tokio`, `common` |

---

## Design Goals

- **Zero input latency impact.** Clipboard operations never run on the input hook thread. All reading and writing happens in dedicated Tokio background tasks.
- **Deduplication.** A SHA-256 hash of each outgoing payload prevents re-transmitting content that the remote side already set.
- **Format coverage.** Handles the most common clipboard formats used by Windows applications.

---

## Supported Formats

| Clipboard Format | Description |
|---|---|
| `CF_TEXT` | ANSI text |
| `CF_UNICODETEXT` | Unicode (UTF-16) text |
| `CF_DIB` | Device-independent bitmap (classic) |
| `CF_DIBV5` | Device-independent bitmap v5 (with color profile) |

---

## Architecture

### Host Side (`sync.rs`)

1. `AddClipboardFormatListener` registers the NerveDesk window to receive `WM_CLIPBOARDUPDATE` messages.
2. On update, a Tokio task reads the clipboard content with `OpenClipboard` / `GetClipboardData`.
3. The payload is hashed. If the hash matches the last-sent hash, the update is dropped (deduplication).
4. Otherwise, the payload is chunked and enqueued for transmission over the file transfer channel (TCP 48106).

### Client Side

1. The receiver reassembles chunks and reconstructs the original clipboard format.
2. `OpenClipboard` / `SetClipboardData` / `CloseClipboard` apply the content.
3. The locally applied hash is recorded to suppress the resulting `WM_CLIPBOARDUPDATE` echo.

---

## `format.rs`

Helper utilities for reading and writing individual clipboard formats, including BITMAPINFOHEADER parsing for DIB/DIBV5 images.

---

## Related

- [NET.md](NET.md) — file transfer channel used for clipboard payloads
- [ENGINE.md](ENGINE.md) — spawns clipboard sender/receiver workers
- [ARCHITECTURE.md](../ARCHITECTURE.md) — clipboard in the system overview
