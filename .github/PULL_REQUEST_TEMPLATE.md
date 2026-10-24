## Summary of Changes

A concise explanation of the changes made, the problem solved, and architectural decisions involved.

Fixes #(issue number)

## Subsystem Impact

- [ ] `common` / Protocol Definitions
- [ ] `input` / Win32 Low-Level Hooks & Input Emulation
- [ ] `capture` / DXGI & Windows Graphics Capture
- [ ] `codec` / Hardware Video Encoder & Decoders
- [ ] `net` / Transport Protocols (UDP/TCP/TLS) & Cryptography
- [ ] `engine` / Background Services & Coordination State
- [ ] `ui` / Tauri v2 Frontend & Window Lifecycle
- [ ] `driver` / IddCx Virtual Display Driver

## Architectural Checklist

- [ ] **Protocol Compatibility**: Wire format remains backward-compatible or version is incremented.
- [ ] **Zero-Allocation Hot Path**: Input and video streaming loops avoid unnecessary heap allocations.
- [ ] **Thread Safety**: No blocking Win32 message calls inside async Tokio workers.
- [ ] **Cryptographic Invariants**: Nonces are strictly monotonic; DPAPI secret storage is maintained.
- [ ] **Documentation / ADR**: New architectural choices are documented in `docs/adr/` or `docs/specs/`.

## Testing & Verification

Describe the manual and automated tests executed to verify these changes:
- [ ] `cargo check --workspace` passes with zero warnings.
- [ ] `cargo test --workspace` passes.
- [ ] Tested end-to-end on Windows 10/11 hardware.
