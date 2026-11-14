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

## Code audit and refactor

2026-11-14. The owner requested a full review of `crates/` and `drivers/`, repeated
after changes until the result was satisfactory. The review covered the codec,
resource and session engine, both control hosts, both native adapters, the Windows
driver and their tests/build boundaries. No physical SSD was captured during this
refactor. The audit concluded with no outstanding regression found in the changed
paths; this is bounded test evidence, not proof of universal USB support.

The first pass removed production fixture assumptions and redundant interfaces.
The next pass consolidated transport and validation, then split large dispatch,
worker and bootstrap functions into private operations. Subsequent reviews checked
allocation order, cancellation ownership, native destruction and API migration,
using portable tests followed by native Windows tests. A final review checked the
resulting source and test evidence after those corrections.

Changes:

- Production macOS commands are `list`, `inspect` and `serve`. Device-specific
  qualification commands and constants moved into the `qualification` example.
  Journal target parsing accepts validated generic identities; capture still
  binds an exact registry generation. Production source no longer refers to
  milestones or specific test devices.
- The codec has one admission-aware `encode` and `decode` API. Incremental and
  blocking readers also require admission. Borrowed submit/completion frames
  remove temporary payload copies. The engine has one backend-factory constructor;
  invalid configuration is rejected before acquiring native resources.
- `knightbus-transport` owns bounded TCP framing, backpressure, absolute partial
  frame deadlines and failure shutdown for both adapters. The Windows forwarder
  inherits its budget and timeout from its wire connection.
- `knightbus-windows-protocol` is a dependency-free `no_std` crate shared by the
  Windows importer and driver. It replaces source-file inclusion and duplicate
  channel constants, and provides tested attachment/reply validation. Descriptor
  parsing is split into focused private operations.
- Windows IOCTL dispatch, URB submission, native worker pumps, forwarding and
  bootstrap now use smaller private functions. The passive owner gate, cancellation
  lock, successful-unmark ownership transfer and endpoint-parent queue lifetime
  remain explicit. Both control hosts share subscription ownership and cleanup.

Review corrections:

1. Returning the complete parsed topology from the shared attachment parser
   produced a 5,160-byte kernel stack frame. The API now returns only a 31-byte
   endpoint map and borrowed descriptors. The driver build also checks functions
   in the shared protocol crate. The final build passes all 68 checked functions
   with a 4 KiB maximum per frame; this is not a bound on the complete call stack.
2. Bootstrap must admit descriptor inventory before sending SET_CONFIGURATION.
   Review restored that ordering before the final tests.
3. The open-client retirement test waited 20 seconds for a process whose cleanup
   deadline is 30 seconds. It now allows 45 seconds, while still requiring the
   kernel reservation to remain held until the external client closes. The test
   passed in 31.69 seconds, including the expected cleanup-timeout error.

Validation:

| Check | Result |
| --- | --- |
| macOS workspace, all targets | 110 passed, 1 opt-in test ignored |
| Linux workspace, all targets, Rust 1.85 | 103 passed, 1 opt-in test ignored |
| Windows workspace, all targets, fresh build directory | 102 passed, 12 opt-in tests ignored |
| macOS release suite and doctests | 111 passed, 1 opt-in test ignored |
| Clippy, warnings denied; rustfmt; rustdoc, warnings denied | PASS |
| Crash recorder/finalization Python tests, including example journal ACK | 5 passed |
| Old/new descriptor parser comparison | 58,339 mutations/truncations matched acceptance and topology |
| Decoder fuzz smoke run | 13,211,602 inputs, no crash |
| Stream fuzz smoke run | 2,085,607 inputs, no crash |
| Native SuperSpeed publication/retirement and owner/malformed-buffer rejection | PASS |
| Native owned-controller retirement, partial attach and owner/network failure | PASS |
| Native WinUSB large bulk, stall/reset and setting reselection | PASS |
| Native retirement with an external open client | PASS |
| Native independent-controller owner-exit isolation | PASS |
| Native bounded slots, cross-thread release and unprivileged-name rejection | 2 passed |

Linux used Apple's `container` 1.7.1 and the cached `rust:1.85.0-slim-bookworm`
image. There were no container compatibility, setup or execution issues. The
descriptor comparison harness and fuzz runs are additional audit evidence; the
retained unit/integration tests cover framing, malformed input, admission, limits,
ownership and cleanup. Opt-in tests are counted separately from portable passes.

Windows builds used the existing WDK, Rust toolchain, certificate and test-signing
configuration. Source archive timestamps again required explicit target-specific
cleaning of the driver and shared protocol crate; portable tests used a fresh
`target-verified` directory. A later SSH shell lacked Cargo on PATH; adding the
existing user Cargo directory resolved it before tests ran. No OS security or boot
setting was changed. The temporary WinUSB fixture package was removed. The tested
KnightBus package remains staged, with its current removal record at
`C:\Users\billy\KnightBusLab\refactor\installation.json`.
The previous M9 record was archived as `installation-before-refactor.json`.
Final inspection found no importer process, synthetic/controller PnP nodes or
M9/refactor scheduled tasks. Existing Windows feature-upgrade tasks were left
unchanged. `session-closed.json` records this state and signed package hashes.

Logs are retained in Mac `/tmp/knightbus-refactor` and Weasley
`C:\Users\billy\KnightBusLab\refactor`; Windows logs are also copied into the Mac
directory. The tested unsigned driver SHA-256 is
`516FD686CAB151A0DA7BFD4BCCD2703BCBE20EB4EFE4296495213E7943032C0F`.

Source API migration is intentional: replace `*_with_admission` with the single
codec entry points and pass admission explicitly; construct the engine with a
backend factory; pass BOS explicitly to `Descriptors::new` (empty for USB2); use
the four-argument `Forwarder::new`. The Windows command flag is now `--insecure`,
replacing `--insecure-lab`. Build/run macOS fixture commands through
`cargo build -p knightbus-macos-exporter --example qualification`; the production
binary rejects them. These changes do not add Windows ISO, static streams,
multiple-configuration or remote-wake support, and do not close the physical
integrity, reconnect, hot-unplug or soak gates above.

## Physical repeat after refactor

The same SSD was passed through again using the refactored exporter, importer and
tested driver. Windows mounted it read/write as `SDD_Backup (F:)`, reported Healthy
and not dirty, and Explorer was verified visible in billy's session. The owner
reported finishing. Before stopping forwarding, the volume was no longer mounted
in Windows. Both ends then confirmed native retirement with zero reserved bytes
and requests. No importer process or selected USB/controller PnP nodes remained.
The exact volume UUID remounted at `/Volumes/SDD_Backup`; the temporary importer
task and SSH tunnel were removed. No automated hash-integrity test was run.

Evidence is in Mac `/tmp/knightbus-refactor-ssd` and Weasley
`C:\Users\billy\KnightBusLab\refactor-ssd`. The PowerShell launcher's process
exit-code record was empty; cleanup was independently verified from the native
retirement log and absence of processes/PnP nodes. The launcher task returned zero.

The next requested device, Anker PowerConf C200 (`291a:3369`, high speed), was
inspected without capture. Its video interface 1 alternate 1 uses isochronous IN
endpoint `0x88`; microphone interface 3 alternate 1 uses isochronous IN `0x82`.
The current shared Windows validator rejected the actual descriptors with
`invalid endpoint or unsupported transfer type`. Windows isochronous transport
and driver support are required before a video trial. No camera stream or Windows
camera attachment was started. Descriptor evidence is in `/tmp/knightbus-webcam`.

## Throughput diagnosis and scheduling correction

The owner observed about 5 MB/s during the repeated SSD trial. Both hosts reported
1 Gbps Ethernet links. The receive loop consumed at most one 16 KiB chunk before
returning an incomplete frame, and the importer slept for 1 ms after every tick.
The exporter also treated partial frame input as idle. This imposed a scheduling
ceiling on large frames independently of the SSD's native speed. The same behavior
existed before the refactor. The Windows physical repeat also used a debug importer.

The transport now drains ready reads and short writes within a fixed limit of
16 I/O steps and 256 KiB per call, and reports partial progress to its host. Busy
hosts yield and continue; idle hosts retain a short sleep. The 16 KiB scratch
allocation, single output slot, admission checks and absolute frame deadlines are
unchanged. A receive call returns immediately on one complete frame: USB request
sizes do not increase, and small reports are not held to fill a batch. Cancellation
still precedes new submissions. These bounds limit CPU monopolization; concurrent
physical HID latency under storage load remains unmeasured.

The temporary loopback benchmark used for these measurements transferred 8 MiB
per frame size, checked payloads and sequence acknowledgments, and verified final
budget release. Its implementation has since been removed from the repository;
the results below remain as historical evidence.

| Frame payload | Mac before | Mac after | Windows after, release |
| --- | --- | --- | --- |
| 4 KiB | 2.9 MB/s | 3.2 MB/s | 2.7 MB/s |
| 64 KiB | 10.1 MB/s | 51.7 MB/s | 41.1 MB/s |
| 1 MiB | 12.5 MB/s | 543.8 MB/s | 649.5 MB/s |

These are individual loopback measurements, not SSD/network throughput promises.
Small sequential exchanges still encounter idle wakeup delays. Actual storage
performance also depends on BOT serialization, network/tunnel latency and workload.
No physical SSD transfer was rerun after this correction.

Regression tests exercise ready large frames, bounded short reads/writes,
coalesced small frames, idle/backpressure activity, unchanged deadlines and
cancellation priority. Workspace/all-target checks passed on macOS (113 tests),
Linux through Apple `container` (106), and Windows release (105). Opt-in tests were
excluded from those counts. Native Windows controller retirement and 1 MiB bulk
transfer/stall/reset tests passed separately. Clippy and rustdoc with warnings
denied, formatting and diff checks passed. There were no container issues.

Logs are in Mac `/tmp/knightbus-throughput` and Weasley
`C:\Users\billy\KnightBusLab\throughput`. The temporary WinUSB fixture was removed;
no importer or synthetic controller/device remains active. The updated release
importer for the next physical trial is
`C:\Users\billy\KnightBusLab\throughput\target\release\knightbus-windows-importer.exe`.
No kernel driver or private ABI change was needed.

### Physical result and Windows worker timing

The owner repeated a large-file copy from `F:` to Weasley's local disk with the
release builds and reported only about 7 MB/s. A ten-second Windows counter sample
measured 7.41 MB/s, approximately 48 disk reads/s and 156 KB per read. Average read
latency including queueing was 264 ms, with about ten requests queued. These are
disk-level counters, not individual USB transfer timings.

A separate 64 MiB synthetic stream over the same SSH tunnel reached 92.28 MB/s.
TCP echo round trips were 0.493 ms median and 0.804 ms p95. Neither USB nor disk
access was involved in that probe. Weasley reports QEMU with four logical CPUs;
both Ethernet interfaces advertise 1 Gbps. The first scheduling fix therefore
removed one limit but did not resolve the physical storage bottleneck.

A second probe measured the Windows native worker's 1 ms condition-variable
timeout: 15.416 ms median and 15.618 ms p95. Ordinary thread sleep for the same
requested duration measured 1.528 ms median. The worker polls new native requests
after this timeout, so the coarse wait adds avoidable delay between USB operations.

The worker now uses a latched auto-reset event for mailbox/stop notifications and
a private high-resolution waitable timer for idle polling. Signals arriving before
the wait remain pending, and notifications interrupt the timer immediately. This
uses the documented [Windows high-resolution timer](https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-createwaitabletimerexw)
available since Windows 10 1803, without changing system timer settings. Timer
creation/wait failures propagate through worker cleanup; no kernel ABI changed.

The replacement measured 1.524 ms median and 1.537 ms p95 on Weasley. Notification
tests cover pre-wait signals, coalescing, single consumption, cross-thread wakeups
and timer rearming. Importer tests and Clippy pass on macOS; importer tests pass
on Linux through Apple `container` without execution issues. Windows release
workspace/all-target tests pass (107 tests); native owned-controller retirement
and large bulk/stall/reset tests also pass. The synthetic WinUSB package was
removed after testing. Evidence is in `/tmp/knightbus-throughput-diagnosis` and
Weasley `KnightBusLab\throughput-timer`.

The owner ejected the SSD, and the first throughput session retired with exit code
zero and zero reservations at both ends. The volume UUID remounted on the Mac;
the importer task and tunnel were removed. A new physical trial is active
with the timer-corrected release importer: `F:` is healthy, writable and not dirty,
and Explorer is visible. Evidence is in Mac `/tmp/knightbus-timer-ssd` and Weasley
`KnightBusLab\timer-ssd`. No physical speed result for that build is recorded yet.

### Event-driven forwarding after the timer trial

The owner measured only 8–9 MB/s with the timer-corrected build. A bounded Windows
UCX trace enabled IRP, performance and header keywords (`0x64`), without payload
keywords. It captured 3,422 completed transfers. Most data requests were 128 KiB;
the 31-byte BOT commands averaged 5.06 ms, 13-byte status reads 5.15 ms, and
128 KiB data reads 7.52 ms. These three phases account for roughly 7.4 MB/s.
The median completion-to-next-submit gap was 0.004 ms. The long delays were within
the forwarded operations, not a delay before Windows submitted the next command.
The trace session was stopped after sampling; evidence is in `timer-ssd` on
Weasley and `/tmp/knightbus-throughput-diagnosis` on the Mac.

Both hosts now wait on socket readiness and native progress notifications instead
of a timer between operations. `knightbus-transport` uses Mio 1.2.2 with only its
polling/network features. The core backend contracts accept an optional standard
Rust `Waker`; unsupported backends retain polling. Native adapters signal lifecycle,
request, cancellation and completion changes. The standalone importer exposes
`wait()` between bounded ticks, with a 10 ms housekeeping cap. Kernel arrival
polling still uses the private high-resolution timer. There is no driver/ABI change.

Small frames are delivered immediately; batching never waits for a larger payload.
The 16 KiB scratch buffer and 256 KiB/16-step work bounds remain. A fixed 16 KiB
allowance covers poller bookkeeping and remains charged while any backend retains
the wake owner. Tests cover wakeups before waiting, late callbacks after stream
close, socket readiness, idle sockets, short reads/writes and backpressure recovery.
Review caught continuously enabled writable interest on Windows; writable events
are now requested only after backpressure, with a test preventing idle spin.

The release loopback benchmark measured 4 KiB frames at 146.0 MB/s on the Mac and
144.9 MB/s on Weasley, compared with about 3 MB/s under timed polling. Windows
64 KiB and 1 MiB results were 1195.5 and 579.8 MB/s respectively. These short runs
measure scheduling/framing and vary between runs; they do not establish physical
SSD throughput. Native controller retirement and large bulk/stall/reset tests pass,
including the standalone event-wait path. Portable tests, Clippy, formatting and
documentation checks pass; Linux used Apple `container` without execution issues.

The timer trial was ejected and closed with zero reservations and no remaining
controller nodes. Its task and tunnel were removed, and the same UUID remounted
on the Mac. The next physical trial uses `KnightBusLab\throughput-events` release
binaries and records session evidence under Mac `/tmp/knightbus-events-ssd` and
Weasley `KnightBusLab\events-ssd`. The owner measured about **26–40 MB/s**, depending on the file, copying
one large file from F: to Weasley’s local disk, up from 8–9 MB/s with the timer
fix and about 5 MB/s initially. This is an Explorer-reported result, not a
controlled benchmark. It supports forwarding waits as a major bottleneck; the
remaining throughput limit has not yet been isolated.

### Required wakeups and native Windows notification

A subsequent metadata trace captured 5,862 completed transfers. Command/status
means fell to 1.22/1.14 ms. Most read data requests were now 512 KiB (1,354
requests, mean 9.07 ms); the trace also contained 510 writes of that size (mean
25.50 ms), so it is not a controlled read-only comparison. The transfer size
variation helps explain file-dependent speeds. Remaining native arrival polling
was identified in the Windows worker; its contribution is not separately timed.

Both core backend traits now require wakeup registration, using the shared
`WakeSource` implementation. The optional capability flag and forwarding fallback
are removed. Native Windows arrivals and cancellations signal a referenced event
through the owner-only NOTIFY IOCTL. The worker shares that event with replies
and shutdown and waits without a polling timer. Tests cover registration after
publication, replacement/unregistration, reentrant scheduling, latched wakeups,
idle parking and notification-driven cancellation. Native tests additionally
check owner, event type, handle permissions, malformed input and retirement.

The previous SSD session was ejected and closed with zero reservations; the
same UUID remounted on macOS and its task/tunnel were removed. Build, test and
package evidence for the notification change is under `/tmp/knightbus-async-notify`
and Weasley `KnightBusLab\async-notify`. Portable suites passed: macOS 120, Linux 113, Windows 112 tests. Linux used
Apple `container` without issues. Clippy, formatting, rustdoc and the 70-frame
kernel stack check passed. Windows initially reused a stale test artifact; a
fresh `target-verified` build ran the complete suite and the actual new native
notification test. Native notification authorization/cleanup, owned retirement
and large bulk/stall/reset tests all passed. The signed driver was restaged using
the existing certificate; its current record is `async-notify\installation.json`.
The temporary WinUSB fixture was removed.

The asynchronous SSD session was recorded under `async-ssd`: F: was healthy,
writable, not dirty and visible in Explorer. The owner reported an average of **46 MB/s** for the read-copy trial, compared
with the previous 26–40 MB/s range. This is an Explorer-reported observation;
file and cache differences prevent attributing the full increase to notification
changes. After owner eject, both programs retired native ownership with zero byte/request
reservations. Windows had no remaining controller/device nodes; the same volume
UUID remounted on macOS. The importer task and SSH tunnel were removed.

The owner then tested the reverse direction with the same asynchronous build in
`async-write-ssd`. They reported similar throughput in both directions, with
F: → Weasley appearing steadier. No separate numeric write measurement was given.
Caching, SSD write behavior and host activity remain possible explanations for
the variation; the cause was not isolated. After owner eject, this session also closed with zero byte/request reservations,
no remaining Windows controller/device nodes, and the same UUID remounted on the
Mac. Its importer task and SSH tunnel were removed.

## Shared native contracts and worker consolidation

The follow-up refactor moves portable behavior out of both OS crates. Compared
with the pre-refactor working tree, their `src/` trees contain 3,029 fewer lines
(macOS 1,438; Windows 1,591). This measures relocation and simplification of OS
code, not an equal reduction in the whole repository. Existing uncommitted work
was preserved.

- `knightbus-core::exporter` owns the physical worker, endpoint order, topology
  barriers, cancellation, native retirement and native-request validation.
  The native driver reports its actual selected endpoint before submission.
- `knightbus-core::importer` owns typed native requests, sequence/token correlation,
  fair batching, cancellation delivery, selected alternates, storage admission
  and event-driven scheduling. Native implementations provide OS operations and
  a required latched signal. `ThreadSignal` supplies the callback-based version.
- Both driver contracts require explicit `Capabilities`. The importer pipeline
  derives wire limits and checks descriptors before native publication; storage
  is reserved before the native factory runs. Native requests and completion
  owners cannot bypass the attachment's aggregate budget.
- `knightbus-transport` now owns both complete data paths, including descriptor
  bootstrap, configuration verification and forwarding. Its importer `connect`
  function composes the pipeline with one budget. Shared forwarding uses the core
  importer lifecycle rather than another attachment/drain state machine.
- `knightbus-usb` provides allocation-free USB descriptor validation to userspace
  and the kernel. Windows-specific limits remain in `knightbus-windows-protocol`.
  Regression tests prove that generic acceptance of remote wake, multiple declared
  configurations and larger descriptor trees does not relax the Windows policy.
- Worker/bootstrap tests moved alongside their shared implementation. OS crates
  no longer re-export duplicate transport/backend modules. The
  [adapter guide](native-adapters.md) describes the narrow extension contracts.

Repeated review passes checked the extraction, ownership boundaries, native
capability limits, wakeups and final API composition. Follow-up corrections added
foreign-budget rejection, fail-closed importer clock regression handling and
native request/snapshot checks. The kernel stack checker was extended to include
`knightbus-usb`, so moving a parser cannot move it outside stack verification.

One new Linux test initially counted idle iterations while cancellation completion
was still progressing. Cancellation ownership and idle parking now have separate
fixtures. The final Linux suite and ten additional runs of all six importer
contract tests passed. Apple `container` had no compatibility, setup or execution
issues. The Windows encoded-command launcher exceeded the command-line limit for
the combined native script; copying and invoking that script resolved it without
changing the test procedure.

Final validation:

| Check | Result |
| --- | --- |
| macOS workspace/all targets, Rust 1.85, locked/offline | 130 passed; 1 opt-in test ignored |
| Linux workspace/all targets in Apple `container`, Rust 1.85 | 123 passed; 1 opt-in test ignored |
| Linux importer contract repetition | 60 additional passes across 10 runs |
| Windows workspace/all targets, fresh release build, Rust 1.85 | 122 passed; 13 native/opt-in tests ignored in this invocation |
| Native notification owner/type/access/cleanup | 1 passed, 0.33 s |
| Native owned-controller retirement, including partial attach/owner faults | 1 passed, 12.67 s |
| Native SuperSpeed publication/retirement | 1 passed, 0.98 s |
| Native 1 MiB bulk/STALL/reset forwarding | 1 passed, 1.33 s |
| Windows driver locked/offline build and stack inspection | 71 driver/protocol/USB frames, each at most 4 KiB |
| macOS exporter release build, locked/offline | Passed |
| Workspace/driver formatting, Clippy with denied warnings, rustdoc with denied warnings, diff whitespace | Passed |

The signed driver is staged as `oem3.inf`. Its current ownership record is
`C:\Users\billy\KnightBusLab\shared-refactor-final\installation.json`;
`previous-installation.json` retains the old record for evidence only. The temporary
WinUSB fixture (`oem4.inf`) was removed after its test, and no live KnightBus
controller remained. Signed SYS SHA-256:
`136191A34545F5904DA190E908344B623204A490E24AC94AD1273874D0508BFE`.
Build and test logs are under that Windows directory and locally under
`/tmp/knightbus-shared-refactor/`.

This refactor does not expand the supported native USB feature set. The shared
importer still rejects ISO and uses one selected configuration; Windows streams
and remote wake remain unsupported. Physical SSD throughput was not remeasured
in this change, and the remaining M9/M10 qualification gates are unchanged.

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
& C:\Users\billy\KnightBusLab\refactor\target-verified\debug\knightbus-windows-importer.exe `
  --insecure 127.0.0.1:43249 kb-0000000002220000 `
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


## Local lab separation

Manual hardware fixtures, native lab tests, crash recorder instrumentation,
development signing/install helpers, and the development driver INF were moved
out of `crates/` and `drivers/` into the gitignored repository-root `lab/` tree.
Earlier commands and results in this report describe the retained lab versions.
The macOS product library no longer includes the crash journal. The Windows
product importer uses only attachment-owned controllers; legacy preinstalled
controller support, object-observation counters, and numeric lab trace hooks
were removed. Standalone lab tools depend on the actual crates; implementation
copies and alternate workspaces were removed. Product builds and CI do not require `lab/`.
Ordinary automated regression tests and fuzz targets remain tracked.
