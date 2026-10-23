# ADR-003: Windows DPAPI for Pairing Key Storage

| Field | Value |
|---|---|
| Status | Accepted |
| Date | 2026-10-20 |
| Deciders | NerveDesk core team |

---

## Context

The pairing key is the root secret from which all channel encryption keys are derived. It must be stored persistently so that the user does not need to re-enter it after restarting the application.

Storage options considered:

| Option | Risk |
|---|---|
| Plaintext file | Readable by any process running as the same user |
| WebView localStorage | Accessible to any JavaScript running in the WebView |
| OS keychain (Windows DPAPI) | Protected by the Windows user account; inaccessible from other accounts |

---

## Decision

The pairing key is stored using **Windows Data Protection API (DPAPI)** via `CryptProtectData` / `CryptUnprotectData`.

- The encrypted blob is written to `%APPDATA%\NerveDesk\pairing-key.bin`.
- Encryption is tied to the current Windows user account and machine. Only the same user on the same machine can decrypt it.
- The `CRYPTPROTECT_UI_FORBIDDEN` flag prevents DPAPI from showing a consent dialog.
- The same mechanism protects the TLS private key for the mobile remote server (`mobile-{ip}.key`).
- The NerveDesk UI (`secret_store.rs`) is the sole interface for reading and writing the key; the WebView layer never receives the plaintext key.

---

## Consequences

**Positive:**
- Key is invisible to JavaScript, browser devtools, and any other process running in a different user session.
- Key survives application reinstallation (the `%APPDATA%` directory is preserved).
- No user-managed keyfiles or passwords required.

**Negative / Accepted:**
- Key is lost if the user's Windows account is deleted or the machine is reimaged without backing up the `%APPDATA%` directory.
- On a compromised machine (malware running as the same user), DPAPI does not protect the key — this is an accepted limitation of user-space key storage on any platform.
- Not portable across machines — each installation generates its own DPAPI-encrypted blob.
