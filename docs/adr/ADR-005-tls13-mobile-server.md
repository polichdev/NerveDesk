# ADR-005: TLS 1.3 and PIN Authentication for Mobile Remote

| Field | Value |
|---|---|
| Status | Accepted |
| Date | 2026-10-22 |
| Deciders | NerveDesk core team |

---

## Context

The mobile remote feature exposes a local HTTPS server so smartphones and tablets can view the screen and control input without installing an application. This introduces a browser-accessible HTTP endpoint on the local network.

Requirements:
- Traffic must be encrypted — PIN and session tokens must not travel in plaintext.
- The server must be accessible from any browser without installing a custom root CA.
- Unauthorised devices must not gain access, even if they are on the same Wi-Fi network.
- Brute-force PIN guessing must be rate-limited.

Options considered for transport security:

| Option | Problem |
|---|---|
| Plain HTTP | PIN and session token visible to any LAN observer |
| Self-signed cert (TLS) | Browser shows a warning; user must accept |
| Let's Encrypt / trusted CA | Requires a domain name and internet access — not viable for LAN |

---

## Decision

The mobile remote server uses **TLS 1.3** with a self-signed certificate generated per local IP address.

**Certificate management (`mobile_tls.rs`):**
- `rcgen` generates a certificate valid for `localhost` and the current local IP address as a Subject Alternative Name.
- The private key is encrypted with Windows DPAPI and stored at `%APPDATA%\NerveDesk\mobile-{ip}.key`.
- On restart, the encrypted key is decrypted and the same certificate is reused (avoids generating a new fingerprint on every start).
- `fingerprint(ip)` returns the SHA-256 DER fingerprint for display in the UI so users can verify they are connecting to their own machine.

**TLS configuration (`rustls`):**
- Protocol version restricted to **TLS 1.3 only** (`with_protocol_versions(&[&TLS13])`).
- No client certificate authentication.

**PIN authentication (`mobile_server.rs`):**
- A 6-digit PIN is generated via `BCryptGenRandom` at startup.
- `POST /api/verify` accepts the PIN and returns a 32-byte crypto-random session token on success.
- Session tokens expire after **1 hour**.
- **Rate limiting:** five failed attempts from one IP address trigger a **5-minute block**.
- PIN regeneration clears all active session tokens.

**Endpoint access control:**
- `/screen.jpg`, `/stream`, `/api/input` — require a valid session token.
- `/api/status` — public but returns the local IP only to authorised sessions.
- `/` — always accessible (serves the web UI HTML, which then requires a token for any data).

---

## Consequences

**Positive:**
- All traffic (PIN, token, screen capture) is encrypted with TLS 1.3.
- Rate limiting prevents automated PIN brute-force within a reasonable attack window (5 attempts per 5 minutes).
- DPAPI-protected private key is not accessible from other Windows user accounts.

**Negative / Accepted:**
- Browsers display an "untrusted certificate" warning because the cert is not issued by a public CA. Users must click through or add an exception.
- The phone does not automatically verify the PC's identity — a MITM on the LAN could present a different self-signed cert. Users should compare the fingerprint shown in the NerveDesk UI before entering the PIN.
- The certificate is bound to the local IP. If the machine's IP address changes, a new certificate is generated and the browser exception must be re-accepted.
