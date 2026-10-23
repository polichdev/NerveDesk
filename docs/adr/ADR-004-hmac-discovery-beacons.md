# ADR-004: HMAC-SHA256 for Paired Discovery Beacons

| Field | Value |
|---|---|
| Status | Accepted |
| Date | 2026-10-23 |
| Deciders | NerveDesk core team |

---

## Context

Device discovery uses UDP broadcast beacons on port 48103. These beacons are plain JSON and serve two purposes:

1. **General discovery** — any NerveDesk instance on the subnet can see other instances and list them in the UI.
2. **Paired peer communication** — once two machines are paired, the host sends direct unicast beacons to the peer's IP. The peer uses these to update screen-edge layout and confirm the peer is online.

The original design sent all beacons as unsigned plaintext. This means:

- Any device on the LAN can spoof a beacon pretending to be the paired peer.
- A spoofed beacon can change the receiver's stored screen-edge configuration.
- In hostile LAN environments, beacon flooding can pollute the device list.

---

## Decision

A two-tier beacon authentication model is applied:

**Broadcast beacons (unsigned):** sent to `255.255.255.255:48103` without any signature. This preserves the zero-configuration discovery experience — a new device can find existing instances without a pre-shared secret.

**Unicast beacons to the paired peer (HMAC-SHA256 signed):** sent directly to the paired peer's IP. The wire format is:

```
[ payload_len: 4 bytes LE ] [ JSON beacon payload ] [ HMAC-SHA256: 32 bytes ]
```

The `DiscoveryListener` on the paired peer:
- Accepts unsigned beacons for general discovery (`pairing_key = None`).
- When the sender IP matches the configured `paired_peer_ip`, the beacon **must** carry a valid HMAC. Unsigned beacons from the paired peer IP are rejected.

Comparison uses `constant_time_eq` to prevent timing side-channels.

---

## Consequences

**Positive:**
- Beacon spoofing is prevented for the authenticated paired-peer channel.
- General discovery remains zero-configuration for unpaired devices.
- The HMAC key is the existing pairing key — no additional secret is required.

**Negative / Accepted:**
- Broadcast beacons remain unauthenticated. Any device on the subnet can see the host's name, IP, and screen dimensions. This is intentional for discovery but leaks metadata to passive observers.
- Adding HMAC to every broadcast would require all discovering devices to hold the pairing key before they can even list available hosts — breaking the discovery UX.
