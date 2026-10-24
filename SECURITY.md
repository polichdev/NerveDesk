# Security Policy

## Supported Versions

Only the latest release of NerveDesk receives active security updates and vulnerability patches.

| Version | Supported          |
| ------- | ------------------ |
| 1.0.x   | :white_check_mark: |
| < 1.0   | :x:                |

---

## Security Architecture & Design Principles

NerveDesk is designed with strict local-first security principles:

1. **Local Network Isolation:**
   - NerveDesk does not route your keystrokes, mouse movements, clipboard data, or screen captures through external cloud relay servers.
   - All communication takes place strictly within your local network (LAN / Wi-Fi).

2. **Authenticated transport between PCs:**
   - Configure the same randomly generated pairing key on both PCs. The key is stored with Windows DPAPI for the current user, not in WebView `localStorage`.
   - Input, virtual-display video, screen mirror, audio, clipboard, and file-transfer payloads use ChaCha20-Poly1305 authenticated encryption. Receivers also restrict connections to the configured peer IP. Legacy plaintext media and file protocols are rejected.
   - Folder-sync setup is encrypted with the pairing key. After approval, sync sessions use the pair's challenge-response proof and encrypted records. File hashes and version history remain in place.
   - A pairing key is a shared secret, not a device certificate. This version does not provide forward secrecy for PC-to-PC traffic. Anyone who obtains that key can impersonate the paired PC; rotate it on both PCs after exposure.

3. **Mobile remote:**
   - Mobile access uses TLS 1.3 on a local HTTPS server. A self-signed certificate is generated per local IP; its private key is protected with Windows DPAPI. Browsers warn because the certificate is not issued by a trusted authority. The phone does not automatically verify the PC's identity, so confirm you are opening your own PC before entering the PIN.
   - The PIN and one-hour session tokens use the Windows cryptographic random generator. Five failed PIN attempts from one IP block further attempts for five minutes. Regenerating the PIN or stopping the service clears session tokens.

4. **Network boundary:**
   - The optional Windows Firewall rule allows only private-profile traffic from the local subnet. Network discovery beacons are unauthenticated and may be spoofed; discovery alone does not grant data-channel access.
   - For hostile LANs or access across networks, use an encrypted tunnel and restrict firewall access to the intended peer. Manual pairing on both machines remains required.

---

## Reporting a Vulnerability

We take the security of NerveDesk seriously. If you discover a security vulnerability or potential exploit, please report it responsibly:

1. **GitHub Private Advisory (Preferred):**
   - Navigate to the **Security** tab of the repository and click **"Report a vulnerability"** to submit a private report.
2. **Issues:**
   - If private vulnerability reporting is unavailable, open a GitHub Issue with minimal disclosure details and request a private communication channel.

Please include:
- A description of the vulnerability and its potential impact.
- Steps to reproduce or a proof of concept (PoC).
- Your operating system and NerveDesk build version.

We will acknowledge receipt of your report within 48 hours and work on a patch promptly.
