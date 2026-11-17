# KnightBus for Windows x64

## Install

1. Extract the ZIP, then double-click **KnightBus-Setup-x64.exe**.
2. Approve the administrator prompt using the same Windows account you normally
   use. This development build is unsigned; Windows may show an unknown-publisher
   warning. Verify that you are running the file you received from this build.
3. Open **KnightBus** from Start. The installer starts the background service;
   the app should connect automatically. The background service has no terminal
   window. Do not run the GUI as administrator.

Programs and this guide are installed in `C:\Program Files\KnightBus`.
The installer configures the background service to run with administrator access
at this account's login. Closing the app leaves forwarding
running; signing out ends it. Installation starts no USB sharing or attachment.
Pairing state stays in `%LOCALAPPDATA%\KnightBus`.

Requires Windows 11 x64, [Microsoft Edge WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)
and the [Microsoft Visual C++ v14 Redistributable (x64)](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist/).
If setup reports a missing runtime, install it from Microsoft and run setup again.

The ZIP includes the Microsoft-signed usbip-win2 UDE driver. Setup installs it
with the app and service; no separate driver download or test mode is needed.
Restart Windows if setup requests it. The driver license and matching source are
in `driver/`. The KnightBus installer itself is not yet publisher-signed.

The Windows adapter supports compatible USB 1.x, USB 2 and SuperSpeed devices
with one configuration. Physical SuperSpeedPlus devices use the driver's virtual
USB 3 port without changing the physical connection speed. Remote wake and bulk
streams are unsupported; storage devices need a compatible Bulk-Only Transport
mode. See `architecture.md` for the adapter limits.

## Find your computers

Install this discovery update on both computers. Open **Computers → Find nearby**.
Both can be found before pairing or sharing a device. Discovery lists nearby computers; use a PIN to pair them.

**Make computer discoverable on LAN** in Settings must be enabled. The installer
adds a firewall rule for the KnightBus service's mDNS traffic (UDP 5353), restricted
to the private local network. Windows must classify your trusted home network as
Private. It also permits the service’s temporary TCP pairing listener from the
private local subnet. The listener closes after pairing, cancellation or expiry.
Guest Wi-Fi isolation, VPN routing and separate subnets may prevent mDNS;
a manual pairing address is available below the PIN. Connection codes remain under Advanced pairing.

To update an earlier build, disconnect devices and wait for **Disconnected**,
then choose **Quit UI** from the tray and run this setup executable. You do not
need to uninstall. Setup stops the existing service gracefully, replaces the
programs and restarts it. Your identity, paired computers and UI preferences stay
in place; setup verifies and installs the pinned USB/IP driver.

## Connect to your Mac

Use a matching current KnightBus app/service on the Mac, with native USB capture
permissions configured there.

1. Open **Add a computer** on the Mac and click **Show PIN**.
2. On Windows, open **Add a computer**, click **Find computers**, select the Mac,
   enter its six-digit PIN and click **Pair**. Both computers are paired together.
   The PIN expires after two minutes, accepts at most five attempts and is single-use.
3. On the Mac, select **Share…** beside a local device, select the Windows computer
   and confirm **Share device**. On the first share, approve macOS administrator setup so KnightBus can capture USB devices; the UI stays unprivileged and pairings are preserved. Unmount storage before sharing it. For a removable drive, allow the macOS removable-volume permission when prompted. This permission covers removable drives, not just the selected drive; KnightBus still shares only devices you select. If denied, enable **Removable Volumes** under **System Settings → Privacy & Security → Files & Folders → KnightBus**, then retry. Full Disk Access is not needed.
4. On Windows, under **Remote devices**, select the Mac and click **Connect**.
   If its address is missing, enter the Mac's LAN IP.
5. Choose **Use device**. Check **Computers → Connections** for **In use**.
6. To finish, choose **Disconnect** and wait for **Disconnected**.

On macOS, closing the UI leaves sharing running. After a reboot or **Quit
KnightBus**, use **Start local service** in the app. Administrator approval is
needed for initial setup or repair. Existing pairings are preserved.

The default Mac listener ports are TCP **3337** and **3338**. Allow those ports on
its trusted private LAN if a firewall blocks the connection. Pairing and LAN
advertisement alone never grant access to a USB device.

## Service recovery

In **Settings → Advanced settings**, the installed service endpoint is
`\\.\pipe\knightbus-control`. Select it and reconnect if the app remembers a
previous local companion endpoint. The **Start local service** button restarts the installed account’s service when
present, preserving its identity and driver access. If startup is denied, rerun
setup with administrator approval.

If the installed service is stopped, open PowerShell as administrator and run:

```powershell
Start-ScheduledTask -TaskName 'KnightBus daemon'
Get-ScheduledTaskInfo -TaskName 'KnightBus daemon'
```

Then use **Retry connection** in the app. If setup fails, its Details panel shows
the error. Do not delete an existing installation while it owns USB devices.

## Uninstall or replace this build

Disconnect every device, wait for **Disconnected**, and choose **Quit UI** in
KnightBus's tray menu. Open Windows **Settings → Apps → Installed apps**, select
**KnightBus** and uninstall using the original installing
account. You can also run `C:\Program Files\KnightBus\Uninstall.exe`.

Uninstall removes the programs and login task. It retains pairing state, the
separate USB driver and the installing account marker. Setup recognizes the
unchanged driver files left by earlier uninstallers and reinstalls normally.
Unrecognized files in an existing installation folder are preserved. For upgrades, simply quit the tray app and run the new setup.
See `architecture.md` beside this file for supported platforms and adapter limits.

## Quit completely

Use **Quit KnightBus** in the app header or tray menu to disconnect devices,
stop the daemon and close the UI. It waits for cleanup; a timeout keeps the app
open. **Quit UI (sharing continues)** still closes only the UI.

The app checks for the installed USB/IP controller before remote capture. If it
is missing, rerun setup. If the driver cannot start, check Device Manager and
restart Windows. After a daemon crash, a restart may be needed to release stale
virtual devices. Normal **Quit KnightBus** disconnects them cleanly.
