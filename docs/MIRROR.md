# Screen mirroring without a display driver

The current portable `NerveDesk.exe` can transmit the PC's existing screen to
another Windows computer over the local network. This is **Mirror Primary
Display**, not an independent second Windows monitor.

1. Run the same `NerveDesk.exe` on the PC and the laptop. Put both devices on
   the same LAN; Ethernet on the PC is supported.
2. On the PC, open **Screen Mirror** (**Дублювання екрана**), click
   **Refresh** (**Оновити**), and select the laptop from the device list.
3. Choose **Entire primary screen** or a specific open window, such as Word.
   Window capture can continue while another window covers Word on the PC;
   keep Word open and do not minimize it.
4. Click **Start streaming** (**Почати трансляцію**). Keep NerveDesk running on
   the laptop to receive video. Allow NerveDesk through Windows Firewall if asked.

The receiving laptop displays the stream full screen. NerveDesk preserves the
source screen's aspect ratio and centers it with black bars when the screens
have different shapes. Changing the source or receiver resolution does not
require restarting the stream. The source pixels remain at native resolution
up to 3840×2160; the receiver scales them to its display. Screen mirroring
shows the same desktop on both devices, not a separate Windows workspace.

Mirroring uses TCP port 48104 on the receiving laptop. Both machines must run
the updated NerveDesk build. If the device appears but video does not, check
that the laptop's firewall allows inbound TCP 48104 on the private network.
The older driver-backed stream still uses UDP 48101.

The portable application does not include the IddCx driver. An independent
second monitor needs a Microsoft signed driver package while Secure Boot
remains enabled. See [DRIVER.md](DRIVER.md).
