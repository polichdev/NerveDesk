# ADR-002: ChaCha20-Poly1305 for Authenticated Encryption

| Field | Value |
|---|---|
| Status | Accepted |
| Date | 2026-10-20 |
| Deciders | NerveDesk core team |

---

## Context

All PC-to-PC data channels (input, video, audio, file transfer, folder sync) require:

1. **Confidentiality** — payloads must not be readable by anyone on the LAN who does not hold the pairing key.
2. **Integrity and authenticity** — a packet must be rejected if it was tampered with or injected by a third party.
3. **CPU efficiency** — the cipher must not impose significant overhead on the encoding and streaming hot path.

Candidates evaluated:

| Cipher | Notes |
|---|---|
| AES-128-GCM | Fast on CPUs with AES-NI; slow in software without it |
| AES-256-GCM | Same AES-NI dependency |
| ChaCha20-Poly1305 | Constant-time on all CPUs; no hardware instruction dependency |

---

## Decision

All data channels use **ChaCha20-Poly1305** as the AEAD construction, provided by the `chacha20poly1305` crate (RustCrypto).

**Key derivation:**
```
cipher_key = SHA-256( "NerveDesk v2:" || context || pairing_key )
```
The domain-separated `context` string (e.g. `b"input"`, `b"audio-packet"`, `b"folder-sync:…"`) ensures that keys derived for different channels are independent even if the same pairing key is reused.

**Nonce generation:** `BCryptGenRandom` (Windows CSPRNG), producing a fresh 96-bit nonce per packet.

**Packet layout:**
```
[ nonce: 12 bytes ] [ ciphertext ] [ Poly1305 tag: 16 bytes ]
```

---

## Consequences

**Positive:**
- Constant-time on all x86/x64 CPUs regardless of AES-NI support.
- Widely audited cipher (IETF RFC 8439).
- Single 256-bit key covers both stream cipher (ChaCha20) and MAC (Poly1305).
- Domain separation prevents cross-channel key reuse attacks.

**Negative / Accepted:**
- No forward secrecy. The pairing key is a long-lived shared secret. Any historical traffic intercepted by an adversary can be decrypted if the key is later compromised. Rotate the pairing key on both machines after any suspected exposure.
- A CSPRNG nonce collision is theoretically possible after 2^48 packets on the same key/context pair (birthday bound), which is negligible for local-network use.
