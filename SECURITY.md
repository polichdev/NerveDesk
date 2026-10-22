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

2. **Dynamic Pairing & Ephemeral Tokens:**
   - Pairing requires an alphanumeric security key exchanged between devices.
   - Mobile remote access is protected by an in-memory 6-digit random PIN and ephemeral session tokens.
   - Clipboard payloads and file transfer handshakes are validated against paired machine tokens.

3. **Untrusted Networks Caution:**
   - Some local network streams (such as uncompressed audio bridge and raw folder synchronization data) are designed for trusted private subnets. Always connect via a trusted home/work network or an encrypted point-to-point tunnel (e.g., WireGuard/Tailscale) when operating outside home environments.

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
