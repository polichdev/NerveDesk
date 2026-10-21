# NerveDesk virtual display driver

The PC host uses its own IddCx UMDF driver at `driver/idd-virtual-display/`.
The driver exposes one Windows monitor and sends BGRA swap-chain frames to the
NerveDesk application at `127.0.0.1:48103`. The application encodes and
transmits the frames to the laptop over the existing LAN video transport.
The laptop only runs the NerveDesk receiver; it does not need the display driver.

Running `cargo build` or `cargo run` produces an application without the driver
package. In those builds, the UI reports that the driver is not bundled and
does not offer installation. The release packaging script requires a complete,
signed INF/DLL/CAT package before it can create the installer.

## Build and release requirements

1. Build `NerveDeskDisplay.vcxproj` in Release|x64 with Visual Studio, the
   Windows Driver Kit and a matching Windows SDK. The INF targets Windows 11 x64.
2. Generate a catalog from `NerveDeskDisplay.inf` and sign the driver package
   for release through Microsoft's driver signing process. Do not ship a
   test-signed package to end users.
3. Put `NerveDeskDisplay.inf`, `NerveDeskDisplay.dll`, and
   `NerveDeskDisplay.cat` together in a directory.
4. Run `tools/package-windows.ps1 -SignedDriverDirectory <directory>`.
   The script checks the catalog signature and file membership, then bundles
   the three files with the Tauri installer under `resources/driver/`.

The Windows installer asks whether this computer will send an extended desktop.
Select **Yes** on the PC and **No** on the receiving laptop. The PC installer
creates a root-enumerated device and installs the bundled driver with its
administrator privileges. The same action remains available through the
**Install NerveDesk driver** button in the app. No third-party driver or
download is involved.

## Status

The Rust application and an x64 IddCx DLL were built locally with Visual
Studio Build Tools 2022 and the Microsoft WDK 26100.6584 NuGet package. The INF
passed Inf2Cat validation. A development certificate was used to test-sign a
local package under `target/wdk/test-bundle/driver/`. This is **not** a release
signature, and the driver has not been installed or tested end-to-end on a PC
and laptop. On the current host, Windows rejected both an unsigned catalog
(`0x800B0100`) and an untrusted development certificate (`0x800B0109`) while
Code Integrity was enforced. These attempts left no NerveDesk device installed.
Do not publish a release until the driver receives Microsoft-approved signing,
passes the relevant HLK checks, and is tested on a clean PC and laptop.
