# Failure Modes, Resilience & Recovery Runbook

| Field | Value |
|---|---|
| Target System | NerveDesk Hybrid KVM & Display Streaming Suite |
| Document Classification | Operational Reliability Guide |
| Scope | Network Disruptions, Driver Crashes, Input Lockouts, Port Conflicts |

---

## 1. Input Channel & Lockout Resilience

### 1.1 The "Stuck Keys" Problem
**Failure Scenario**:
A user holds down `Ctrl + Shift` while dragging a window across screen edges. At the exact moment the cursor transitions to the client machine, the Wi-Fi disconnects or packet loss surges. If the `KeyUp` event packet is lost over UDP, the client operating system will believe `Ctrl + Shift` remain held down indefinitely.

**NerveDesk Mitigation & Recovery**:
1. **Heartbeat Timeout Sweep**:
   - The receiver (`input` crate) maintains an active bitmask of currently depressed virtual keycodes.
   - If no UDP input or keepalive datagram is received for **800 milliseconds**, the receiver triggers an automatic **Panic Release**:
     ```rust
     // Emits synthetic KeyUp for all held modifier and standard keys
     emulator.release_all_held_keys();
     ```
2. **Physical Emergency Escape**:
   - Pressing `Ctrl + Alt + Shift + Escape` locally on the host machine instantly detaches the low-level Windows hook (`UnhookWindowsHookEx`) and forces cursor capture back to the primary screen.

---

## 2. Display Driver Crashes & Adapter Loss

### 2.1 DXGI Device Removed / Reset
**Failure Scenario**:
The Windows graphics driver crashes and restarts (TDR - Timeout Detection and Recovery), or an external GPU is detached during display casting. `IDXGIOutputDuplication::AcquireNextFrame` returns `DXGI_ERROR_DEVICE_REMOVED` or `DXGI_ERROR_DEVICE_RESET`.

**Recovery Procedure**:
1. The capture worker catches the device removal HRESULT.
2. The Direct3D 11 device, context, and output duplication interfaces are completely released.
3. The engine waits 500 ms for the Windows display driver to finish TDR re-initialization.
4. A brand-new Direct3D 11 device is instantiated via `D3D11CreateDevice`.
5. Capture automatically resumes without requiring the user to restart NerveDesk.

### 2.2 IddCx Virtual Monitor Disappearance
**Failure Scenario**:
The virtual display driver is disabled via Device Manager or uninstalled while an active streaming session is targeting it.

**Recovery Procedure**:
- The capture engine detects invalid monitor handles (`MONITORINFOEX` lookup fails).
- The engine automatically reverts streaming to the primary physical desktop (`\\.\DISPLAY1`) and emits a warning toast notification in the UI.

---

## 3. Network Disruptions & Port Conflicts

| Failure Symptom | Underlying Cause | Automatic Engine Response | Manual Recovery Step |
|---|---|---|---|
| `Failed to bind UDP :48100` | Another instance of NerveDesk or third-party KVM is running. | Process displays a message dialog indicating port conflict and aborts cleanly. | Run `Get-Process NerveDesk` in PowerShell and terminate orphan instances (`Stop-Process -Name NerveDesk -Force`). |
| Video stream freezes while mouse input remains responsive | TCP video connection dropped by Wi-Fi AP while UDP input continues to route. | Video client detects TCP EOF/reset, cleans decoder queue, and initiates reconnect loop with exponential backoff (1s, 2s, 4s). | Toggle the "Display Cast" switch off and on in the GUI. |
| Discovery beacons not received | Windows Defender Firewall blocked public network profile. | NerveDesk embeds automated CLI firewall rules (`NerveDesk.exe --firewall-allow`). | Run `NerveDesk.exe --firewall-allow` from an elevated Administrator PowerShell prompt. |

---

## 4. Secret Store & Pairing Desynchronization

**Failure Scenario**:
Windows DPAPI encryption fails due to a domain account password reset or corrupted local application data, causing saved security keys to fail decryption.

**Recovery Procedure**:
1. NerveDesk detects DPAPI decryption failure and safely logs `DPAPI decryption error; clearing invalid cached credentials`.
2. The user interface opens the "Pairing" tab displaying an unverified status.
3. The user re-generates or re-enters the 6-character pairing key on both machines to establish a fresh shared secret.
