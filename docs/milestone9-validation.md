# M9 standalone physical storage trial

2026-11-14. **MANUAL FILE TESTS PASS; CLEAN DETACH VERIFIED**, not milestone acceptance.
The physical SSD was mounted read/write as `F:\` on Weasley and visible in
Explorer in billy's desktop session. The owner reported finishing and ejecting it.
KnightBus released the connection, and the same volume is mounted back on the Mac.
Root-directory enumeration succeeded;
no existing user files were intentionally changed by the test runner.
The owner selected the
Mac-connected SSD for native Windows Explorer file operations, then approved
extending the current USB 3.2 path rather than reconnecting through USB 2.0.

## Selected hardware and scope

The volume is actually named `SDD_Backup`, ExFAT, on a 256.1 GB physical USB disk.
Its Ugreen/Realtek bridge is `0bda:9210`, physical location `0x02220000`, connected
at 10 Gbit/s. The unmodified descriptor reports USB 3.2 and one configuration:
interface 0 alternate 0 provides Bulk-Only Transport (BOT); alternate 1 provides
UAS with stream-capable bulk endpoints. macOS initially used its UAS driver.

The intended first Windows path is native BOT over the existing physical
SuperSpeedPlus connection. Static streams remain unsupported. Microsoft documents
that [UdeCx reports static streams unsupported](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/udecxwdfdevice/nc-udecxwdfdevice-evt_udecx_wdf_device_query_usb_capability).
No descriptors are rewritten to hide UAS or pretend that the physical device is
USB 2.0. Windows must select a supported setting; stream-bearing settings are
rejected before forwarding SET_INTERFACE. This does not qualify general UASP,
ISO, USB4 tunneling, suspend/resume or arbitrary USB devices.

## Implementation

- Shared descriptor validation accepts USB/IP SuperSpeed and SuperSpeedPlus,
  checks EP0 rules and mandatory endpoint companions, tracks stream requirements
  per alternate, and validates a bounded BOS tree. The same parser runs in the
  importer and original kernel driver.
- The importer retrieves the physical BOS via ordinary USB/IP control requests.
  Private attach envelope version 2 appends BOS after the length-delimited
  configuration; version 1 remains unchanged for existing USB2 tests. An old
  driver rejects the new envelope instead of misinterpreting it.
- UDE receives the physical device/configuration/BOS descriptors and its
  SuperSpeed device class. USB/IP speed 6 remains recorded for the physical
  connection; this is not a claim of a 10 Gbit/s virtual link or network throughput.
- The Mac standalone host binds loopback only, selects one exact registry
  generation, grants only that selected device to the explicit insecure lab
  principal, and routes discovery/import/transfers/unlink through `Engine`.
  TCP input/output and native storage share bounded admission. EOF drains the
  native lease; stale generations are not automatically recaptured.
- The Windows development runner accepts `until-stop=ABSOLUTE_PATH`, allowing
  manual storage use without an automatic test-duration disconnect. Create that
  file only after applications have closed the volume and Windows has safely
  ejected it.

## Validation evidence and current limits

Portable tests cover SuperSpeed descriptor truncation/companions/BOS, grant
filtering before capture, fragmented/coalesced real TCP traffic, bulk IN/OUT,
UNLINK and EOF retirement. macOS and Linux workspace/all-target tests pass.
Linux used Apple's `container` 1.7.1 with the cached Rust 1.85 image; no container
compatibility, setup or execution issue occurred. Windows portable tests pass.
Native tests remain separately opt-in.

The initial native synthetic SuperSpeed publication failed. Windows USBHUB3 ETW
reported descriptor validation failure on USB2 port 1: UDE's automatic port
selection had not selected the USB3 root port. An explicit USB3 port number 1
was rejected; the root hub uses combined numbering (USB2 port 1, USB3 port 2).
The corrected driver selects port 2 for SuperSpeed and port 1 for USB2.
No physical SSD capture occurred during these synthetic trials.

With explicit USB3 port 2, the native synthetic SuperSpeed child enumerated and
retired successfully. The endpoint-zero fixture needed to STALL the optional
Microsoft OS string request, rather than assert that every request was
SET_CONFIGURATION. The existing native owned-controller test then passed three
USB2 CDC rounds plus partial-attach/owner/network fault cleanup. These native
results qualify virtual publication and regression behavior, not SSD filesystem
use. The updated kernel build passed its 60-function, 4 KiB stack-frame check.

The first physical SSD trial cleanly unmounted all local volumes and acquired
the device, but interface opening was denied by macOS TCC under
`kTCCServiceSystemPolicyRemovableVolumes`. The log identifies the system launchd
process and `deny iokit-open-service IOUSBHostInterface`. Native release completed,
the engine returned zero bytes/requests, and macOS automatically remounted the
volume with a new registry generation. No Windows attachment or filesystem test
occurred. The continuation uses an interactive Terminal host to allow the normal
macOS permission prompt. No TCC database or security policy is edited.

The interactive launch was attributed by TCC to Visual Studio Code. TCC recorded
approval, but the initial prompt took three seconds and exceeded the native
two-second interface-claim deadline. This trial also retired with zero budget.
The next attempt captured successfully, retrieved the physical 42-byte BOS,
published USB/IP speed 6, and forwarded SET_CONFIGURATION, SET_INTERFACE(0),
GET_MAX_LUN and bulk storage traffic. Windows then bugchecked; this is a failed
physical trial, not a successful filesystem test.

The retained dump `ssd-crash.dmp` / `111426-11125-01.dmp` reports
WDF_VIOLATION `0x10d/0xa`: UdeCx called WdfIoQueuePurge on a queue with
`STATUS_DELETE_PENDING` (`0xc0000056`). The inherited endpoint-configuration
callback explicitly deleted released queues. Release notifications do not
establish final destruction: UdeCx/UCX subsequently purged one during recovery.
The correction removes that deletion and leaves final queue retirement to its
endpoint parent. Queue cleanup still clears the fixed ownership table. Native
owned-controller retirement passed (12.61s), as did the extended WinUSB stall,
reset, repeated setting selection and bidirectional echo regression (1.99s).
The temporary WinUSB fixture package was removed after the test. The debugger
could read WDF/UdeCx stacks and the fatal-error structure, but the symbol server
did not provide this OS kernel's matching symbols; full analysis is limited.

Development evidence is retained in Mac `/tmp/knightbus-m9` and
Weasley `C:\Users\billy\KnightBusLab\m9`. This includes failed synthetic logs,
USBHUB3 ETW, signed packages, exact installation records and build/test output.
The Windows build uses the existing development certificate and test-signing
configuration. No boot-security or machine-wide PowerShell policy is changed.
The initial PowerShell runner issues (script policy, then stderr redirection
being treated as a terminating native-command error) were resolved with a
process-scoped execution override and CMD-owned log redirection.

## Further physical findings and final handoff

The first detached PowerShell launcher was tied to its SSH command lifetime.
The current importer runs in the manually started `KnightBusM9SSD` scheduled
task, using billy's interactive elevated token, with no execution-time limit.
It is not configured to start at boot or recapture a device automatically.

Native disk enumeration then exposed an unsupported 397,312-byte read. The
transfer ceiling is now 1 MiB throughout both wire adapters and the Windows
private channel, matching the Mac native backend. Kernel payload storage is
charged as 31 MiB plus metadata before creating the controller; the standalone
Windows budget is 96 MiB. Incoming TCP scratch storage remains 16 KiB. Requests
above the ceiling fail; this does not promise arbitrary USB transfer sizes.

A clock mismatch invalidated the first large-transfer trial: archive timestamps
from the Mac were older than Weasley's existing build artifacts, so Cargo reused
the previous binaries. Those initial large-transfer logs are not evidence of the
new limit. The driver package was explicitly cleaned and rebuilt, and the final
importer used a fresh `target-m9-final` directory. Build logs confirm compilation.
Source copies and fresh build directories must be used for subsequent trials;
do not trust a successful cached build after cross-machine archive extraction.

The next failure was a separate MDL length bug. UdeCx returned a 4096-byte mapped
buffer for a bulk URB requesting only 1024 bytes. Forwarding the mapped capacity
consumed the 1024-byte data plus the following 13-byte BOT status packet; Windows
then waited for the already-consumed status. The driver now validates the typed
bulk/interrupt URB and uses `TransferBufferLength`, bounded by the mapped buffer.
The physical trace and one-time numeric kernel diagnostics confirm both
1024-in-4096 and 512-in-4096 cases. No storage-specific protocol translation or
descriptor rewriting was introduced. `KNIGHTBUS_TRACE_USB=1` optionally logs
request/completion metadata for the first 256 sequences, never file payloads.

Final corrected-driver native results: large WinUSB echo/stall/reset/reselection
PASS (2.06s); owned-controller retirement including fault cases PASS (12.84s).
The build passed the 60-function, 4 KiB stack-frame check. Workspace/all-target
tests pass on macOS, Windows (fresh output directory) and Linux via Apple's
`container`; Clippy passes with warnings denied. Native tests are opt-in and
were run separately, not silently counted as passing in the portable suite.
Mac retirement now drops native objects before publishing retirement; a blocked
destructor regression verifies this ordering.

Final physical evidence:

- Native USB Mass Storage binding and WDC PC SN730 disk, Windows Disk 1,
  256,060,514,304 bytes, online and writable.
- `F:` label `SDD_Backup`, exFAT, Healthy; root enumeration returned six entries.
- Explorer COM inspection confirmed visible `file:///F:/` windows in session 1.
- `fsutil dirty query F:` reports not dirty. **Windows C: is dirty after the
  earlier crash**; no repair or reboot was performed after this finding.
- `ssd-volume-proof.json`, `explorer-proof.json`, `native-urb-*.log`,
  `driver-build-urb.log`, `windows-final-tests.log` and the preserved failed
  trial logs are in the Weasley M9 lab directory. The Mac metadata trace
  is `/tmp/knightbus-m9/exporter-interactive.log`.

The owner confirmed that creating, moving and deleting files in Weasley's Explorer
worked as expected, then reported finishing and ejecting the SSD. These file-test
results are owner-reported, not an automated hash-integrity test. The importer confirmed
native detach with zero reserved bytes/requests; no KnightBus or selected SSD PnP
nodes remained on Weasley. The Mac exporter confirmed native lease retirement and
zero reserved bytes/requests, exited, and macOS remounted the verified volume UUID
at `/Volumes/SDD_Backup`. The temporary importer scheduled task was removed and
the SSH tunnel closed. The development driver package remains staged for further
M9 work. Evidence: `session-closed.json` on both machines and
`/tmp/knightbus-m9/disk-after-manual-test.plist` on the Mac.

Hash-verified file round trips, reconnect, hot-unplug
and an agreed soak remain outstanding. The earlier M8 Mac
panic and general UdeCx allocation qualification are not resolved by this trial.

## Reproduce this lab session

Use the existing lab test certificate and signing configuration; build/sign/stage
with `drivers/windows/{build,package,install}.ps1`. Preserve the install script's
exact package record for removal. After copying sources between clocks, use a
fresh output directory (or explicitly clean the original driver package). Do not
replace the installed driver while a disk is in use.

On the Mac, build `knightbus-macos-exporter` in release mode and run `list` to
select the current registry generation for location `0x02220000`, VID/PID
`0bda:9210`. Verify the disk/volume UUID, then cleanly unmount the whole physical
disk. In this trial that was `diskutil unmountDisk /dev/disk4`, with volume UUID
`5117B969-AADC-3A0F-BCB6-80A453F5A39D`. Disk numbers and registry IDs must be
checked again after reconnect. In an interactive authorized Terminal:

```sh
sudo /path/to/knightbus-macos-exporter serve CURRENT_REGISTRY_ID 43249 \
  --capture --insecure-loopback --storage-unmounted
```

The flag acknowledges the unmount; it is not an automatic unmount operation.
Allow macOS removable-volume access for the responsible launching application.
The prepared trial runner in `/tmp/knightbus-m9/run-interactive.sh` additionally
checks the exact UUID, registry generation, VID/PID, location and binary hash.
Keep that Terminal open throughout the Windows session.

Use an authenticated reverse tunnel from the Mac to Weasley:

```sh
ssh -M -S /tmp/knightbus-m9/ssh-control -fNT \
  -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=3 \
  -R 127.0.0.1:43249:127.0.0.1:43249 billy@192.168.88.95
```

Run the importer persistently on Windows, with an initially absent stop file:

```powershell
& C:\Users\billy\KnightBusLab\m9\target-m9-final\release\knightbus-windows-importer.exe `
  --insecure-lab 127.0.0.1:43249 kb-0000000002220000 `
  until-stop=C:\Users\billy\KnightBusLab\m9\stop-importer
```

The trial task invoked `run-import-task.ps1`, which records the importer PID and
stdout/stderr. It has now been removed; recreate it explicitly for another session.
Raw USB/IP itself is unauthenticated; both
ends bind/connect to loopback and the SSH connection supplies remote transport
authentication. Only the selected device is granted to the explicit lab peer.

After the owner closes files and safely ejects the SSD in Windows, create
`C:\Users\billy\KnightBusLab\m9\stop-importer`. Wait for the importer to report
native retirement and the exporter to release the capture before resuming Mac
use. Then stop the SSH tunnel with `ssh -S /tmp/knightbus-m9/ssh-control -O exit
billy@192.168.88.95`. Remove the temporary scheduled task and, when lab testing
is complete, the exact staged driver package using its installation record.
