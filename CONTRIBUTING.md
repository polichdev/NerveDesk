# Contributing to NerveDesk

Thank you for your interest in contributing to NerveDesk. We welcome contributions from developers of all backgrounds who share our commitment to low latency, security, and high engineering quality.

---

## 1. Development Prerequisites

To develop, build, and debug NerveDesk locally on Windows, ensure your machine has:

- **Operating System**: Windows 10 (version 2004 / Build 19041+) or Windows 11.
- **Rust Toolchain**: Rust 1.80+ (stable MSVC target: `x86_64-pc-windows-msvc`).
- **C++ Build Tools**: Visual Studio 2022 Build Tools with the *Desktop development with C++* workload and Windows 10/11 SDK.
- **Node.js**: Node.js 18+ and npm.
- **Driver Development (Optional)**: Windows Driver Kit (WDK) if modifying the `driver/idd-virtual-display/` project.

---

## 2. Setting Up the Workspace

Clone the repository and initialize submodules if applicable:

```powershell
git clone https://github.com/polichdev/NerveDesk.git
cd NerveDesk
```

Verify your Rust toolchain:

```powershell
cargo check --workspace
```

Build frontend assets:

```powershell
node ui/scripts/build-frontend.mjs
```

Run in development mode:

```powershell
cmd /c "cd ui && npm run tauri -- dev"
```

---

## 3. Git Workflow & Commit Conventions

We follow the **Conventional Commits** specification (`<type>(<scope>): <short description>`):

- `feat`: A new user-facing capability or service.
- `fix`: A bug fix in client, host, or UI.
- `perf`: A code change that improves throughput or reduces latency.
- `refactor`: Internal restructure with no user-visible behavior changes.
- `docs`: Documentation, ADRs, or specification updates.
- `chore`: Build scripts, dependencies, or maintenance.

**Example**:
```text
feat(codec): implement zero-copy texture sharing via D3D11 handles
fix(ui): eliminate default titlebar icon upon tray restoration
docs(adr): add ADR-006 for hardware video pipeline selection
```

---

## 4. Architectural Rules & Coding Standards

1. **Keep the Hot Path Zero-Allocation**:
   - The input pipeline (`input` crate) and video capture pipeline (`capture`/`codec` crates) execute up to 120 times per second. Do not perform heap reallocations, string parsing, or JSON serialization in these loops. Use fixed-size buffers, `bytemuck`, and stack values.

2. **Respect Thread Boundaries**:
   - Win32 low-level hooks (`SetWindowsHookExW`) must run on dedicated OS threads with their own message pump (`GetMessageW`). Never invoke blocking GUI APIs inside Tokio async tasks.

3. **Cryptographic Rigor**:
   - Every datagram sent over UDP or raw TCP must be encrypted using ChaCha20-Poly1305 with unique nonces, authenticated using HMAC-SHA256, or protected via TLS 1.3. Plaintext control messages over LAN are prohibited.

4. **Document Architectural Decisions**:
   - If introducing a new library or non-trivial protocol change, add an Architecture Decision Record to `docs/adr/`.

---

## 5. Verification Checklist

Before opening a pull request, run all automated checks:

```powershell
# 1. Format check
cargo fmt --all -- --check

# 2. Static analysis and linter
cargo clippy --workspace --all-targets -- -D warnings

# 3. Unit and integration tests
cargo test --workspace

# 4. Production build check
cargo build --release
```
