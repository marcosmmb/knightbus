# Windows Rust driver: M7 findings

2026-11-13. **M7: PASS for the agreed fixture-based development scope.**
The owner explicitly deferred physical-device and sleep/wake qualification from
this milestone. This is not production release approval or physical-device proof.
The original Rust importer and UDE driver pass the native fixture, fault, isolation,
endpoint-reset and final-reference retirement checks described below. The observed
NCM stall was traced to the Linux test gadget, with the failed stock baseline and
controlled fixture correction retained. See [final acceptance](#final-reference-retirement-and-m7-acceptance-2026-11-13)
and [review corrections](#review-corrections-2026-11-13) for the checks, current
signed image and limits. Earlier sections are chronological
lab records; their failed experiments and then-current gate decisions are preserved.

## Implementation and provenance

`crates/importers/knightbus-windows-importer` is a workspace crate using the M6 `ImporterBackend`
contract. `drivers/windows` is an isolated Rust KMDF/UDE workspace. The
original M3 driver was recovered from the owner's lab; its channel source SHA-256
was `01990b8ddde51cd44ad0ac1e547781fb5e60ab2edab1dd368d738164b58692df`, matching
the committed M3 report. M7 preserves that measured UDE table binding, request
ownership and separate-DPC completion boundary, and replaces the spike's fixed
device protocol with validated descriptors, bounded native slots and a Rust
userspace USB/IP data plane. Synthetic forwarding commands were removed.

No external forwarding implementation was copied, translated, linked or bundled.
The M0 [reference-design observations](reference-projects.md) inform boundaries:
usbip-win2's kernel networking increases kernel exposure, while a userspace
network parser keeps framing/authentication outside UDE. Those observations retain
M0's source-audit limitations. The USB/IP peer is the separately installed Linux
exporter. Baseline checks used its stock module; the later NCM experiment explicitly
backported one upstream Linux fix into a temporary lab module, restored afterward.

Official bindings are pinned to Microsoft `windows-drivers-rs`
`04523040779d80125f280c4f9421f54a6059f470`. Its [README](https://github.com/microsoft/windows-drivers-rs/blob/04523040779d80125f280c4f9421f54a6059f470/README.md)
explicitly describes the project as early development and not yet recommended
for production. Successful compilation is not a support or qualification promise.
KnightBus's original source is MIT; Microsoft SDK/tooling remains separately licensed.

## Measured apparatus and outcomes

- WEASLEY: Windows 11 x64 build 22631, UdeCx 10.0.22621.1, KMDF 1.33/UDE 1.1.
  Existing test signing and kernel debugging enabled, Secure Boot disabled.
- Build: VS 2026 Build Tools 18.9, MSVC 14.51, SDK/WDK 10.0.28000.0, LLVM 17,
  Rust 1.97.1 for the kernel; userspace MSRV 1.85.0.
- Linux exporter: Ubuntu VM, kernel 5.19.0-46-generic after an owner-initiated
  reboot, stock USB/IP tools/`usbip_host`, configfs ACM gadget, full speed,
  `1209:0001`. Export port 43240 was restricted to WEASLEY. These are original
  test fixtures, **not physical USB devices**.
- This Ubuntu kernel omits `CONFIG_USB_DUMMY_HCD`. The lab built the unmodified
  [Linux v5.19 dummy_hcd.c](https://github.com/torvalds/linux/blob/v5.19/drivers/usb/gadget/udc/dummy_hcd.c)
  against the installed 5.19.0-46 headers, then loaded it temporarily. Source
  SHA-256: `97b77412b02cfdceffc57d0cb49f5dbf7ced992099ab52481230c6700f52f731`.
  This GPL test-only USB simulator is not included or linked into KnightBus;
  the stock USB/IP exporter was unchanged.

| Check | Result / limit |
| --- | --- |
| Original synthetic USB CDC device | Inbox `usbser` binding; 20 checked 64-byte bulk OUT/IN roundtrips and normal detach PASS |
| Stock Linux exporter to native Windows serial API | 20 roundtrips PASS with `usbip_host`/`dummy_hcd`; EP0 descriptors and CDC requests traverse stock USB/IP |
| Composite Linux fixture | Two CDC functions, native `Usbccgp`/`usbser`, 40 checked roundtrips and fixture teardown PASS |
| Original `usbip_vudc` fixture | Data transfers passed, Linux kernel cleanup crashed; this fixture path is not qualified |
| Abrupt socket shutdown | Native attachment and KnightBus-owned children/endpoints/queues retire PASS |
| Importer process termination | Owner-file cleanup retires native attachment and owned objects PASS |
| Allocation and ownership tests | Credits retained across blocked cleanup; admission before native open; duplicate/wrong completion rejected; cancellation ordered after delivery |
| Stream tests | Fragmentation/coalescing, unknown sequence rejection before body allocation, poisoned connection after failure, absolute frame deadline |
| Descriptor tests | Truncation/collisions rejected; composite/alternate metadata accepted; ISO/SuperSpeed rejected |
| INF and test signatures | InfVerif `/w`, Inf2Cat, SYS/catalog signing and catalog membership verification PASS |
| Total framework retirement | FAIL: 12 WDF queues after first synthetic attachment; 13 after the Linux attachment; owned-object counters alone hide this growth |
| Physical USB device with M7 code | Deferred at the owner's request in favor of fixtures; M3's physical joystick proof does not substitute for retesting M7 |
| Two independent controllers | Both native CDC devices passed bulk loopback; terminating one owner left the other operational; owned objects retired |
| Native alternate settings | NCM fixture initially failed before configuration; explicit initial configuration/readback repaired binding and SET_INTERFACE alt 0. Baseline traffic/teardown FAIL; later diagnostic NCM traffic/fault cleanup pass with experimental Windows changes and an upstream Linux fix; general alternate-setting support remains unproved |
| Endpoint reset, sleep/wake | Not qualified; physical bus reset explicitly returns unsupported |
| Production signing, Secure Boot/HVCI, HLK and Driver Verifier | Not qualified |

The Linux test exposed a valid unconfigured import state: configuration zero and
zero active interfaces, despite a complete configuration descriptor. The importer
now distinguishes active state from available topology; a portable regression
covers this case and the lifetime of charged inventory storage. A subsequent NCM
trace showed SET_INTERFACE arriving without an initial SET_CONFIGURATION. Import
now selects the sole supported configuration, checks GET_CONFIGURATION, and only
then permits native publication. A mismatched readback poisons the connection;
the portable test covers both accepted and rejected state.

The NCM fixture bound Windows `UsbNcm` after that fix, but did not become a usable
network adapter. After owner termination, the native child remained visible:
the driver had consumed its child handle, reached its five-second owned-queue
wait limit, and quarantined the controller. Debugger inspection showed one owned
EP0 queue with zero pending/driver-owned requests still alive. This is a separate
cleanup failure from the per-attachment internal control queue growth. It is not
acceptable production behavior; do not infer general network-class support from
successful enumeration. Scoped controller removal then stalled; WEASLEY was
restarted at 20:37:03, after which exact devnode/package removal succeeded.
The lab driver was installed afresh for the final CDC regression, not silently
recycled to count the failed NCM run as passing. The final composite regression
passed 40 transfers, and the synthetic owner-exit/network-loss test passed again.
A repeated NCM run waited explicitly for network initialization but still failed
(`Invalid` from the runner), left native devices present, and required another
restart for rollback. No NCM test in that baseline is counted as passing; the later diagnostic experiment
below establishes traffic and fault cleanup with explicit limitations. A regression also fixes
error reporting: failed native retirement now returns `Cleanup`, even if an
earlier malformed native response caused the worker to stop; its native memory
charge remains quarantined after worker exit.

The earlier `usbip_vudc` experiment used kernel 5.19.0-45 and transferred data, but
its shutdown oopsed in `vep_dequeue`/`vudc_shutdown` at 19:40:41, followed by a CPU
soft lockup. The owner reported a frozen console and rebooted it. The replacement
`dummy_hcd`/`usbip_host` CDC fixtures completed cleanup without that failure.

## Kernel failure found and repaired

The first M7 native run bugchecked WEASLEY at 19:23:52 on 2026-11-11 with
`0x7f / 8` (double fault). Matching dump/image/PDB analysis showed an approximately
11,552-byte unoptimized IOCTL frame nested above UDE removal/hub registry work,
exhausting the 24 KiB kernel stack. The original failing image and dump were
preserved in the lab; the affected package was removed before replacement.

Descriptor validation and attach now have separate non-inlined entry points;
kernel development builds use optimization level 1 and retain debug symbols.
Measured `ioctl` and `remove_child` frames are 568 bytes each, `attach_physical`
224 bytes, and descriptor storage/validation 3,128 bytes. `build.ps1` checks the
PDB records of every original-driver function against a 4 KiB per-frame cap
(41 functions in that baseline; 55 in the final continuation build). This is a regression guard, **not a proof
of total call-chain stack usage**. Native CDC and fault tests passed afterward
without another reboot during those runs.

## Remaining lifecycle investigation

Debugger `!wdfkd.wdfdriverinfo KnightBusUde 0x271` identifies the retained queues
as `CONTROL_QUEUE_CONTEXT` objects parented to the controller; KnightBus endpoint
queue cleanup/destruction counts balance. This narrows the investigation but does
not establish a Microsoft defect. Never delete an internal OS queue or reuse a
consumed `UDECXUSBDEVICE` handle. No automatic controller recycling or resource
exception is implemented or accepted. Compare supported OS/UdeCx versions and
isolate the retained control-queue lifetime before enabling production import.
A temporary second root controller was created, exercised and removed with its
exact devnode identity. Its retained queue disappeared on controller removal;
the surviving controller still had 15 queues after four attachments (baseline
11). This is evidence for a future lifetime redesign, not permission to recycle
a live shared hub. A temporary `SwDeviceCreate` experiment failed with HRESULT
`0x8007007e` even for an empty software device without the KnightBus driver, so
that managed-callback experiment did not qualify software-controller creation.
The later native-callback prototype below succeeds on this VM; this was not
evidence that the Windows API was unavailable.

## C++ comparison of control-queue retention

On 2026-11-11, an original temporary C++ KMDF/UDE driver reproduced the same
retention without Rust code or bindings. It used official WDK headers and stubs,
the same child/endpoint lifecycle and separate-DPC completion boundary, and a
fixed full-speed `1209:0042` vendor-class fixture with no nondefault endpoints.
The existing signed Rust M7 driver then received identical descriptors through
its private channel. Neither test used a network or physical USB device.

Both ran sequentially on WEASLEY with the OS/framework versions above, each
starting with a freshly installed controller that remained loaded for all five
attach/enumerate/detach cycles. Debugger snapshots counted every `FxIoQueue` in
that driver's WDF hierarchy, separately from its own lifecycle counters.

| Completed cycles | C++ WDF queues | Rust WDF queues | Retained `CONTROL_QUEUE_CONTEXT` objects, each driver |
| --- | --- | --- | --- |
| 0 | 11 | 11 | 0 |
| 1 | 12 | 12 | 1 |
| 2 | 13 | 13 | 2 |
| 3 | 14 | 14 | 3 |
| 4 | 15 | 15 | 4 |
| 5 | 16 | 16 | 5 |

Each driver's own child, endpoint and queue creation/destruction counts balanced
after every cycle, and its USB child disappeared from present PnP devices.
Additional idle snapshots still showed 16 queues and five control contexts.
The vendor fixture had PnP code 28 (no function driver); this comparison proves
bus enumeration and reproduces retention, **not native class-driver operation**.

Rust is therefore not a necessary cause of this specific retention symptom.
A shared lifecycle mistake or framework behavior remains possible; this does
not establish a Microsoft defect or prove either driver correct. Rewriting the
driver in C++ is not a demonstrated fix. The NCM owned-EP0 cleanup failure is
separate and was not tested by this experiment. M7 remains UNPROVEN.

The C++ build passed warnings-as-errors, INF/catalog/signature validation and
the stack-frame check (largest measured original-driver frame: 408 bytes).
Both installations and their exact OEM packages were removed successfully;
the final inventory contained zero KnightBus controller nodes/packages. No
restart was required during this comparison.

Temporary MIT source, harness, binaries, symbols and per-cycle debugger logs
remain outside the repository in WEASLEY's `C:\Users\billy\KnightBusLab\m7-cpp`
and the Mac's `/tmp/knightbus-m7-cpp/remote-evidence`. The Rust image remains in
the existing M7 lab package. SHA-256 identities:

- C++ `driver.cpp`: `2e5142df3de66871695144a5d0dc3612ac7b6e54fbe985a28397d962fb3db656`.
- Signed C++ SYS: `6137f3e27e4212e752ebe0a1822c089b57527f833cfa0590e5ebdf94e30b036b`.
- Signed Rust SYS: `4f9faced0c7ad5f88bad6e493334e1cf655f3d8603e92ca5b3c8e3184fba0974`.

### Lifecycle contract review and supported-OS comparison

The follow-up review checked the original C++ reproducer and the corresponding
Rust paths against Microsoft's UDE contracts:

- [PlugOutAndDelete](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/udecxusbdevice/nf-udecxusbdevice-udecxusbdeviceplugoutanddelete)
  requires PASSIVE_LEVEL and invalidates the device handle. Both successful
  detach paths call it from a passive callback and discard the handle.
- [Endpoint callbacks](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/udecxusbendpoint/ns-udecxusbendpoint-_udecx_usb_endpoint_callbacks)
  require Reset; Start and Purge are optional. Omitting Purge alone is not a
  demonstrated contract violation. Both drivers register Reset and complete URBs
  on a separate DPC as required by the
  [UDE guide](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/writing-a-ude-client-driver).
- [EndpointsConfigure](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/udecxusbdevice/nc-udecxusbdevice-evt_udecx_usb_device_endpoints_configure)
  requires association of new endpoint queues and cessation of use of released
  queues. The drivers associate their own queues and clear released queue handles.
  These contracts provide no client handle or cleanup operation for the retained
  internal `CONTROL_QUEUE_CONTEXT` queue.

Read-only inspection with Microsoft's public symbols located the internal queue
creation in `udecx!UsbDevice_UcxUsbDeviceCreate`. On UdeCx 10.0.22621.1, the
inspected `UsbDevice_EvtUcxUsbDeviceCleanup` routine does not visibly delete that
queue. Together with its controller parent in the WDF hierarchy, this supports
investigating framework queue lifetime. This is an inference from the inspected
binary paths, not an exhaustive correctness audit or a confirmed Microsoft bug.
The review logs are `m7-cpp/review-lifecycle.txt`; a Mac copy is preserved in
`/tmp/knightbus-m7-lifecycle`.

WEASLEY was confirmed as Windows 11 **Pro 23H2**, build **22631.6199**. That
edition/version's [support ended on 2025-11-11](https://learn.microsoft.com/en-us/lifecycle/products/windows-11-home-and-pro).
The owner authorized an
in-place upgrade and confirmed an existing VM snapshot. Preflight found no
pending reboot, about 20 GiB free, TPM ready and SSH configured for automatic
startup. Windows Update returned no offered software updates; its compatibility
records rejected 24H2/25H2/26H2 with `CpuFms` for the generic QEMU CPU identity.
SSE4.2 is exposed, so instruction availability alone does not clear that check.
The owner changed the VM CPU type to `host`; Windows now sees an AMD Ryzen 9
7900. Windows Update retained its old compatibility result, so the original,
signature-verified Microsoft Installation Assistant was run with compatibility
checks enabled. It selected Windows 11 26H2, downloaded and verified its image,
and launched Setup. Fresh compatibility providers reported `NoIssue`.

Setup initially stopped with **0xC190020E** (`DiskSpaceBlockInDownLevel`).
The owner expanded the virtual disk from 96 to 160 GiB. C: was extended after
backing up Windows Recovery to both lab machines and moving its partition to
the disk's end; the restored image hash matched and `reagentc /info` confirmed
recovery enabled. C: then had about 79 GiB free. Only 2.4 GB of generated host
build caches had been removed; the old M3 native images and symbols were preserved.

The first installation attempt rolled back to 23H2 with **0x80070057-0x20003**.
SetupDiag identified an invalid `KB5043080` checkpoint-update package during
SafeOS, not a KnightBus driver failure. A second attempt used the verified
Microsoft installation media with the documented
[`/DynamicUpdate NoLCU`](https://learn.microsoft.com/en-us/windows-hardware/manufacture/desktop/windows-setup-command-line-options?view=windows-11#dynamicupdate)
option, retaining compatibility checks and deferring cumulative-update acquisition.
Its pre-restart stage returned zero with no failed package additions. After the
owner restored the missing OpenSSH server service, SSH confirmed Windows 11 Pro
**26H2, build 26300.9457**, UdeCx **10.0.26100.8972**, no setup in progress and
Windows Recovery enabled (WinRE 10.0.26100.9444). Test signing/debugging remained
enabled. Upgrade and partition logs remain in `KnightBusLab\m7-upgrade`.

On 2026-11-12 the unchanged, hash-verified signed C++ and Rust images were tested
again, sequentially on fresh controllers. Both still had queue totals
**11, 12, 13, 14, 15, 16** after cycles 0 through 5, and exact internal control
context counts **0, 1, 2, 3, 4, 5**. Their owned object counts balanced after each
cycle. The upgrade did not resolve retention. CPU identity also changed to host
passthrough, so this is a repeat on the updated VM, not an OS-only controlled
comparison. Evidence: `KnightBusLab\m7-post-upgrade`, copied to the Mac's
`/tmp/knightbus-m7-lifecycle/m7-post-upgrade`.

At the owner's request, the inherited lifecycle was reassessed without assuming
M1/M3 correctness. A separate original C++ variant used Microsoft's documented
[simple endpoint lifecycle](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/writing-a-ude-client-driver#create-simple-endpoints):
create EP0 before PlugIn, omit dynamic configuration/release callbacks and
readiness polling, and let the endpoint parent retire its queue without explicit
queue deletion. It retained the same descriptors, controller/channel scaffolding
and separate-DPC completions. This isolates lifecycle choices; it is not an
independent rewrite of every controller operation.

The simple variant produced **the same 11-to-16 queue growth and 0-to-5 internal
control contexts**, with balanced owned objects and the USB child absent after
each detach. Thus neither dynamic endpoint callbacks nor explicit released-queue
deletion is necessary to reproduce this symptom. This still does not establish
a Microsoft defect or explain the separate NCM failure. All three installations
and their exact OEM packages were removed; final inventories contained zero
KnightBus nodes/packages. No reboot was needed for these comparisons.

The simple variant passed warnings-as-errors, INF/catalog/signature validation,
and a PDB stack-frame check (largest recorded frame 408 bytes). Its source,
package and logs stay outside the repository at
`KnightBusLab\knightbus-m7-simple` and `/tmp/knightbus-m7-lifecycle/simple-evidence`.
Source SHA-256: `6c01708245805e34a80efca4e4edb0b07a5c57d449e570155839bf1bea0d49fc`;
signed SYS: `9cc438fbd76d36aee4197d9c8d8df4b0375b7364435fc279fe86b66e3e8d87ac`.
The vendor fixture still has no function driver (PnP 28), so these tests prove
bus enumeration and lifecycle behavior, not native application functionality.

## Evidence and validation

Raw dumps, signed binaries, traces and temporary setup helpers stay in the lab,
not the repository, following the owner's evidence-retention preference. On
WEASLEY the M7 lab directory contains `crash-image-111126`, `crash-symbols.txt`,
`stack-symbols.txt`, `wdf-after-one.txt`, `wdf-after-linux.txt` and
`wdf-tree-after-linux.txt`; dump `111126-7921-01.dmp` remains in Windows Minidump.
The later NCM investigation is preserved in `wdf-after-ncm.txt`, `ncm-state.txt`
and `ncm-queue.txt`. These paths describe this run, not a downloadable evidence
bundle.

Workspace debug/release tests, Clippy with warnings denied, and documentation
with warnings denied passed on macOS, Linux (Docker) and Windows with Rust 1.85.
The final cleanup-error regression also passed debug/release tests and Clippy
on all three hosts. The importer adds eleven portable tests and three explicitly ignored native tests.
GitHub CI includes the portable importer automatically through `--workspace`;
it neither installs nor qualifies the separate WDK driver.

Final rollback: WEASLEY restarted at 20:45:27. The exact controller recorded in
`installation4.json` and its verified OEM driver package were removed successfully;
no KnightBus lab controller/package remained. Linux's owned gadget, exporter and
firewall rule were removed, and the temporary `dummy_hcd` module was unloaded.
Lab logs and matching signed images remain available for investigation.

See the [architecture](../decisions/windows-driver-architecture.md),
[IPC contract](../windows-driver-ipc.md) and [lab/rollback guide](../windows-driver-lab.md).

## NCM traffic, cleanup and retention investigation (2026-11-12 continuation)

Elevated Windows SSH, Linux root SSH and Proxmox recovery access were verified.
Proxmox access allowed independent dump capture and recovery of WEASLEY (VM 108).
The tests below separate three failures that previously appeared as one NCM failure.
They qualify a diagnostic experiment, **not the repository's general NCM support**.

### Traffic: three distinct causes

1. **Configuration notification ordering.** A trace taken before detach showed
   endpoint additions for `0x81` and `0x02`, followed by `DeviceInitialize`.
   Interface 1/alternate 1 arrived only with endpoint releases during teardown.
   A temporary experiment inferred a setting from a unique complete endpoint-address
   set and forwarded it through the normal control path. This activates this fixture,
   but cannot identify all valid alternate layouts and is not a product fix.
2. **Our foreign-request context bug.** A kernel dump conclusively places the freeze
   in `channel::urb`: an NCM transmit request forwarded from another Windows driver
   lacked `KnightBusRequest` context. Dereferencing it under the controller spin lock
   panicked; other CPUs then waited on that lock. Device-init request attributes do
   not cover every forwarded WDF request. Allocating missing context in a temporary
   build prevented the freeze. That experiment's allocation-failure path completes
   inline, so it does **not** satisfy UDE's separate-DPC requirement under memory
   pressure. It remains outside the repository pending a safe completion design.
3. **Old Linux exporter bug.** With both Windows experiments applied, the adapter
   became `Up`, but Linux rejected NCM32 packets as having the wrong signature.
   Linux 5.19's exporter submits special control requests twice; the second
   SET_INTERFACE resets the gadget's negotiated format. The existing upstream
   [fix 8b6b386](https://github.com/torvalds/linux/commit/8b6b386f9aa936ed0c190446c71cf59d4a507690)
   applied cleanly to the lab module. No KnightBus protocol workaround was added.
   The test used that temporary module, not the stock 5.19 exporter.

With these changes, all five native Windows NCM pings succeeded. Three subsequent
fault scenarios each verified a 32,768-byte TCP echo over the USB network interface
with SHA-256 `a06fa47c2671def27679fe048a287aeb2823c07a1e15d6395e02b3cec681c73d`.
This demonstrates bidirectional native traffic through the experimental path.
The full-speed software gadget, verbose debug build and short run are not a
throughput/latency or physical-hardware qualification.

### Cleanup: a small independent correction

During unplug, UDE reports released endpoints with the previous interface-change
metadata. Forwarding that notification to an already-closing worker returned
CANCELLED and prevented child/default-endpoint retirement. The driver now finishes
local release-only work successfully after retiring the reported queues, provided
unplug has begun, at least one endpoint was released and no endpoint is being added.
Live configuration changes still require remote completion.

The diagnostic build passed normal detach, importer termination and forced closure
of the active Linux USB/IP socket. Each removed the native PnP child; normal detach
also reported zero remaining userspace budget. The link-loss test returned
`Unavailable`, as expected, rather than hiding the connection error as success.
The first link-loss attempt outlasted its trigger and was excluded; the passing
repeat recorded the exact established socket closed by the orchestrator.
The cleanup correction is the only kernel experiment promoted into product source.
The exact repository driver then passed the three single-controller native tests
(CDC echo/detach, malformed requests/owner isolation, and process/network loss).
Running them together exposed a wrong counter pair in the owner-isolation test;
it now compares endpoint/queue creation with destruction rather than default-pipe
additions. A separate NCM run with the repository driver and stock Linux exporter
confirmed detach, zero remaining budget and no present child; traffic remains
unqualified in that build. Kernel build/packaging and the 41-frame stack-size
check passed, as did Rust formatting and diff whitespace checks.

### Retained resources: measured, still unresolved

After six earlier NCM attachments on the same controller, 40 additional vendor
fixture cycles increased UDE `CONTROL_QUEUE_CONTEXT` objects from 6 to 46 and total
WDF queues from 17 to 57. Ten more cycles increased them to 56 and 67. All 56 child
objects and all 74 client endpoint/queue objects were destroyed; the owned queue
mask was zero. No retention plateau was observed.

An inspected retained queue occupies a `0x440`-byte (1,088-byte) nonpaged pool block.
Across the final ten cycles, the driver-tagged nonpaged allocation count increased
by exactly ten and bytes increased from 2,130,512 to 2,141,392: **1,088 bytes per
attachment**. Paged allocations stayed at 17 / 2,208 bytes. The roughly 2 MiB base
includes the fixed native payload slots; it is not leaked anew on each attach.
These measurements cover this fixture and pool tag, not every possible OS allocation.
Fifty additional cycles retained 53.125 KiB in these queue blocks. This is small in
these short tests, but no bounded lifetime or ceiling has been demonstrated for a
long-running controller. Removing the recorded controller/package removed its WDF
client registration and reduced that pool tag to zero allocations/bytes without
a reboot. Earlier independent C++ and simple-endpoint experiments
reproduced the queue growth, so changing languages is not an evidenced remedy.

### Decision and evidence

M7 remains **UNPROVEN**. The immediate cleanup correction is small; general NCM
support needs request bookkeeping that handles foreign requests and allocation
failure without violating DPC completion rules, plus reliable ordering/identity of
configuration notifications. Guessing alternate settings or silently recycling the
controller is not an acceptable general fix. Controller-per-attachment ownership is
an architecture candidate to evaluate separately for bounded global resources,
process death and stale devnodes; these experiments do not approve it as production.

Temporary sources, packet traces and results remain in `/tmp/knightbus-m7-sequence`
and WEASLEY's `KnightBusLab\m7-next`, outside the repository. The crash evidence is
`ncm-hang-kernel.dmp` with `infer-crash.sys/.pdb`; teardown inspection is in
`ncm-cleanup-kernel.dmp`. The successful diagnostic image is preserved under
`release-experiment`, SHA-256
`422dc33f234a8aadc779ee6dc3d7e8ced9b5f33039688049a866c68e03f9e75c`.
`normal-result.log`, `owner-exit-result.log`, `link-loss-result.log`,
`retention-run.log`, `retention-more/run.log` and `poolused-{before,after}-more.txt`
record the outcomes. The Linux module was restored to stock after the experiment.
Final rollback removed the recorded Windows controller and OEM package (zero
present lab nodes/packages), restored `CrashDumpEnabled=3`, and rebooted WEASLEY;
SSH was verified afterward. Linux has no owned gadget, exporter/echo listener or
lab firewall rule. Dumps and matching diagnostic symbols remain in the lab.


## Actual-request routing and controller lifetime (2026-11-13)

This continuation supersedes the preceding paragraph's statement that only the
unplug correction is in the repository. The kernel now includes the missing
foreign-request context admission and an original filter for actual configuration
requests. It does not infer alternate settings from endpoint sets.

### Request ownership and configuration

Before admission/cancellation publication, foreign requests receive our WDF
context if absent. Allocation failure uses an intrusive list in the documented
IRP DriverContext fields while the request is exclusively owned and noncancelable.
A separate DPC clears those fields and completes the failure; there is no inline
URB completion fallback. A temporary forced-failure build rejected every URB at
that allocation boundary. Its native test observed ten completions, zero
wrong-IRQL completions, and retirement of the owned child/endpoints/queues.
The fault injection and its deliberately modified test remain outside the product.
Ordinary cancellation, malformed input and owner isolation also pass native tests.

The same driver service is installed as a lower filter **only on its controller's
virtual root hub**. Public device properties establish the exact parent, a bounded
registry associates the referenced controller, and ATTACH fails until the filter
is ready. The installer journals the hub/filter state before mutation and removes
only its own filter entry during rollback. No physical hub or USB class-wide
filter is changed. Actual SET_CONFIGURATION/SET_INTERFACE packets are validated,
forwarded through the normal bounded EP0 path, and sent onward to UDE after remote
success. Endpoint configuration notifications perform local lifecycle work only.
Detach closes admission and drains accepted selection requests before deleting
the USB child, sharing the existing finite cleanup deadline.

An initial filter prototype hung root-hub startup because its parallel queue was
zero-initialized with **zero presented requests**. The kernel dump identified the
queued root-hub request and the PnP wait; it was our queue initialization bug.
The corrected queue explicitly permits 32 presented requests. Recovery used the
existing Proxmox access and a snapshot before offline rollback. A separate stall
while processing a large diagnostic dump showed paging/storage stacks, not an
active USB callback; it is not evidence of another USB request defect. Matching
images/dumps remain in the lab. Final-source build and packaging passed, including
55 original-driver stack frames below the 4 KiB per-function cap.

### Updated stock Linux exporter

The Linux VM now boots Ubuntu's authenticated `6.8.0-149-generic` kernel packages
(version `6.8.0-149.149`). Its stock, signed, in-tree `usbip_host` has srcversion
`79E98CC6C68183D7B84A7FF`. Only the `dummy_hcd` test gadget module was built
separately, from upstream v6.8 against matching Ubuntu headers; the exporter was
not patched. The old Linux 5.19 duplicate-special-request workaround is unnecessary.
The existing USB/IP userspace utilities were retained. This is a kernel update,
not a claim that the VM's Ubuntu 22.10 userspace is a supported deployment.

Native Windows NCM traffic now works through actual selection requests against
that stock exporter. Normal detach, importer-process termination and verified
closure of the established USB/IP socket each passed a 32,768-byte TCP echo
(SHA-256 `a06fa47c2671def27679fe048a287aeb2823c07a1e15d6395e02b3cec681c73d`)
and removal of the native child. Network loss reports Unavailable rather than
success. A two-controller CDC test also verified that killing one importer leaves
the other controller transferring data.

One earlier link-loss attempt timed out during its preceding TCP echo, **before**
the socket-cut trigger. Linux's echo process had read all 32,768 bytes. That attempt
is excluded from the passing fault test; without its packet trace the timeout's
cause is not established. The passing repeat does not erase that reliability gap.
The final signed build then passed three packet-captured runs of 20 successive
32,768-byte exchanges each (normal detach, importer termination and verified
socket cut): **60 checked exchanges, 1,966,080 bytes echoed**. The 20-exchange phases
finished within about eight seconds of importer startup. Normal detach reported
zero userspace budget; both fault paths removed the child. No echo timeout recurred.
A separate 40-second harness attempt reached the development runner's configured
30-second pending-transfer deadline after a successful echo; it is recorded as
Deadline, not a successful normal-detach test. The final runs used the original
25-second window. This highlights the development runner's finite request timeout;
idle native requests need an intentional timeout policy in production hosting. These short full-speed
software-fixture tests are not throughput, latency, physical-device or endurance
qualification.

### Explicit controller-per-attachment prototype

The lab evaluated the documented
[SwDeviceCreate](https://learn.microsoft.com/en-us/windows/win32/api/swdevice/nf-swdevice-swdevicecreate)
handle lifetime. A small native userspace helper owns one software controller;
normal handle closure or creator-process termination initiates PnP removal. This
helper is test infrastructure, not a C++ kernel shim or a shipped backend change.
Its returned instance ID is used for all matching. Reusing `slot0` keeps the number
of retained software-controller records at one instead of creating unlimited GUID
records. The exact stale subtree was explicitly removed after testing.

Ten attachments alternated normal detach/creator exit with forced creator death
while the child and channel were still active. All ten removed the controller,
virtual root hub and USB child. With no other KnightBus controller installed:

| Snapshot | UDE control contexts | WDF queues | Tagged nonpaged bytes | Tagged paged bytes |
| --- | ---: | ---: | ---: | ---: |
| Before cycles | 0 | 0 | 0 | 0 |
| Live attachment, cycle 1 | 2 | 14 | 2,092,800 | 2,128 |
| After cycle 1 | 0 | 0 | 0 | 0 |
| After cycle 5 | 0 | 0 | 0 | 0 |
| Live attachment, cycle 10 | 2 | 14 | 2,092,800 | 2,128 |
| After cycle 10 | 0 | 0 | 0 | 0 |

Live snapshots had 119 nonpaged and 17 paged allocations under the driver tag.
Offline matching debugger symbols were loaded before counting. These measurements
bound the observed controller lifetime; they do not account for every OS allocation
or prove arbitrary concurrency. The retained queue problem on a reusable controller
has not been eliminated internally in UDE. Instead, explicit attachment ownership
provides a tested way to release it by ending the controller's lifetime.

**Integration state at this experiment (superseded by the integrated results below):**
the product backend still opens a preinstalled
controller. It must acquire/hold the software-device handle in the actual attachment
owner, bound concurrent and retiring slots, wait for removal before reusing credits,
and recover stale records without disturbing other owners. Do not report the lab
prototype as already shipped or as a production resource-safety PASS.

### Validation and remaining gates

The final signed SYS SHA-256 is
`661a2563e4333cdcad59b03e95207c684556c08df289ae42d004a0b52603e0e5`.
Mac workspace formatting, debug/release tests and Clippy passed. Windows workspace
release tests, Clippy and documentation with warnings denied passed. The final
signed driver passed all three single-controller native tests; the same ownership
implementation passed the separate two-controller isolation test. CI continues to
run portable workspace checks; native/WDK qualification remains opt-in lab work.

M7 remains **UNPROVEN as a production milestone**. The request-context crash and
configuration-identity failure now have implemented fixes and native evidence.
Controller lifetime has a promising measured architecture, with production
integration still required. Physical hardware was deferred at the owner's request;
ISO/SuperSpeed, complete reset/power behavior, HVCI/HLK and production signing remain
outside this qualification. No third-party forwarding driver or copied implementation
was introduced, and no changes were committed.

Evidence remains outside the repository: Mac `/tmp/knightbus-m7-next`, Windows
`KnightBusLab\m7-next` (`owned-*`, `final-*`, `oom-test.log`, `sw-cycles.log`,
`sw-{baseline,live-1,live-10,closed-1,closed-5,closed-10}.txt`) and Linux fixture logs.


The final installed filter also survived a WEASLEY restart: both controller and
owned root hub reported PnP status OK, with SSH restored and no startup hang.
The crash-control values were restored from the pre-recovery hive backup
(including `CrashDumpEnabled=3` and `AutoReboot=1`) and verified after restart.

The post-reboot stock Linux composite CDC test passed 40 native COM roundtrips
across its two functions. Scoped rollback then removed the recorded controller,
its virtual-hub filter and the verified OEM package. Final Windows inventory had
zero present lab nodes and zero KnightBus packages. Linux had no owned gadget,
fixture process, listener or firewall rule; the temporary `dummy_hcd` was unloaded.
Windows, Linux and Proxmox SSH access remained available, both VMs were running,
and the offline recovery filesystem was unmounted. The stock Linux kernel update
and lab evidence were retained. The development plan now records actual-request
routing and the remaining controller-lifetime integration contract.

## Integrated attachment ownership and fixture diagnosis (2026-11-13)

The normal CLI now uses `Controller::owned()`, integrated with the existing backend
worker and admission contract. It reserves one of 64 fixed administrator-only
software-controller slots, creates the controller after admission, validates its
own hub before setting the filter, and retires the entire controller on detach.
Unknown identity or failed removal quarantines the owner and credits. Slot
reservation is based on handle existence, so moving the worker between threads
does not abandon a thread-owned mutex. The package installer now defaults to
`Stage`; persistent root controllers remain an explicit diagnostic option.

Native tests passed partial attach failure, three repeated CDC attachments,
network loss, importer termination, and two simultaneous processes where killing
one did not disrupt the other. The slot test held all 64 reservations, rejected a
65th and recovered the same slot after release on another thread. The stock Linux
composite fixture passed three attachments with two native COM ports and 120 checked
round trips through the integrated lifetime. A separate original five-byte HID
fixture bound Windows' inbox HID game-controller driver; native HID reads checked
50 reports covering X ±16000, Y ±12000 and eight button states. Normal detach
removed the HID nodes/controller and returned the userspace budget to zero.
The initial HID harness needed to tolerate Linux endpoint shutdown during
reconfiguration and open the Windows device through `CreateFile`. WinMM joystick
access did not succeed in this SSH session; only native HID input is claimed.

### Network stall: external gadget timer, not a missing importer read

The earlier untraced NCM timeout reproduced twice on stock Linux 6.8. In the first
capture Windows received 29,200 of 32,768 echo bytes. Its next USB/IP bulk-IN request
(sequence 96, endpoint 1, length 16,384) remained pending while OUT requests kept
completing; Linux TCP retransmissions never produced another IN completion.
In the second failure, Linux kprobes captured `ncm_tx_timeout` calling
`eth_start_xmit(NULL, ...)`, receiving `NETDEV_TX_BUSY` (16), and never scheduling
another timer flush. Windows had received 30,660 bytes. This directly matches the
[Linux NCM timer-retry patch report](https://lkml.indiana.edu/hypermail/linux/kernel/2608.2/02481.html).

For a controlled comparison, the lab built Linux v6.8 `usb_f_ncm` with that timer
retry: advance the deadline by `TX_TIMEOUT_NSECS` and return `HRTIMER_RESTART` on
BUSY. This is a temporary GPL **test-device module**, outside the repository;
neither KnightBus nor stock `usbip_host` changed. Six normal runs, one importer-kill
run and one verified socket-cut run passed **160 checked 32,768-byte round trips**
(5,242,880 bytes echoed), with controller/child removal after every run. Stock
`usb_f_ncm` was restored afterward (`srcversion 5B990D2FD5241710086BC5A`);
`usbip_host` remained `79E98CC6C68183D7B84A7FF`. The corrected module SHA-256 was
`3ef2b05916f74423f9d5e56c0b1a06866c3698bcefdd00947a01b988a0159735`.
This diagnoses the observed fixture failure; it does not qualify physical network
adapters or excuse failures in other exporters.

A separate 45-second native NCM test passed traffic and normal detach after the
idle-request policy fix. Bulk/interrupt reads may legitimately wait for input;
only control responses and cancellation have device-response deadlines. Partial
wire frames and writes still have absolute deadlines. Portable regression tests
cover an aged idle read, cancellation starting its own deadline, and an ignored
cancellation failing and releasing the reservation.

### Endpoint-stall recovery: an additional forwarding fix

A new original WinUSB fixture exposed a real gap: a bulk STALL reached the
application, but `WinUsb_ResetPipe` succeeded without clearing the remote endpoint.
Registering `EvtUsbEndpointReset` alone did not cover the Windows stack's typed
`URB_FUNCTION_CLEAR_FEATURE_TO_ENDPOINT` path. UDE's documented
[automatic handling of standard requests](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/writing-a-ude-client-driver)
includes endpoint-halt clearing.

The existing parent-validated hub filter now forwards both raw and typed
CLEAR_FEATURE(ENDPOINT_HALT) requests through the same bounded control channel.
It validates the selector, zero data length and owned nondefault endpoint, then
continues the original request only after remote success. The native regression
uses inbox WinUSB and a small test-only INF, with no third-party forwarding driver.
Its peer rejects a direct clear with EIO: `WinUsb_ControlTransfer` reports
`ERROR_IO_DEVICE`. A subsequent pipe reset delivers the clear, and 20 checked
bulk round trips pass before full controller retirement.

One observed Windows API limitation remains explicit: `WinUsb_ResetPipe` returned
TRUE even when its internal clear was deliberately rejected (EPIPE or EIO).
That high-level BOOL alone therefore does not prove remote recovery. The regression
checks native control-error propagation, the actual setup packet at the peer, and
working transfers after reset. No physical bus reset is claimed; USB/IP has no
standalone command for it and the backend rejects that operation.

## Final-reference retirement and M7 acceptance (2026-11-13)

**PASS for the agreed fixture-based M7 scope.** The owner explicitly deferred
physical-device and sleep/wake qualification. Those checks are not represented as
passing and remain separate platform work. The public production gates, including
signing/Secure Boot/HVCI/HLK and wider transfer support, are unchanged.

A final held-client test found why PnP absence alone was insufficient. After the
importer removed its controller and exited, an application still holding a native
HID file retained **2,098,000 nonpaged and 1,936 paged driver-tagged bytes**, two
control contexts and thirteen WDF queues. The old cleanup path had already
reported success and released credits. Closing that file released the allocation;
this was retained live ownership, not an allocation that survived final release.

Each fixed controller slot now uses an administrator-only manual-reset event.
Before publishing its interfaces, the driver opens the existing event by validated
software-controller identity and holds its own kernel handle. Only the controller's
final WDF destroy callback signals retirement. The owned adapter checks the new
zero-payload `RETIREMENT_READY` IOCTL before attaching, then requires PnP absence
**and** the destruction acknowledgement before releasing credits. This also
rejects an older driver that cannot provide the handshake. No caller-supplied
pointer or handle crosses the private boundary.

An application can legitimately hold a removed device indefinitely. A ten-second
removal deadline therefore returns `Cleanup` and conservatively quarantines the
owner's reservation; it does not pretend the allocation is gone. After importer
exit, the kernel still holds that slot name until the final USB client closes.
The global 64-slot ceiling remains enforceable across processes. This is a bound
on controller instances, not a claim that every Windows USB allocation is charged
to the portable byte budget. The explicit persistent-root-controller path remains
diagnostic only.

Final checks on WEASLEY (Windows build 26300) and Linux kernel 6.8.0-149:

- All three `native_owned_` tests passed: repeated CDC attachment/partial failure,
  two-process isolation, and a native COM file held open during both network loss
  and importer termination. The held-client test observes the unsignaled event
  after the importer exits, then requires its signal after the client closes.
- The 64-slot limit and release on another thread passed. A 65th reservation is
  rejected, and a released slot becomes available again.
- Native HID checked 50 varying input reports. A short-held file allowed normal
  cleanup once closed; holding it beyond the deadline produced the expected
  quarantine. While the importer was gone and the HID file still open, the event
  remained unsignaled and package removal was rejected. Final client closure
  signaled retirement. Debugger snapshots changed from the retained allocation
  above to **zero `Knig`-tagged pool bytes**, with the driver absent from the WDF
  client list. This does not assert zero system-wide USB memory.
- Stock Linux USB/IP and the composite CDC fixture passed three attachments,
  two native COM functions and 120 checked round trips with the updated driver.
- Inbox WinUSB passed the stall/error/reset regression and 20 bulk round trips.
- With the previously diagnosed NCM test-device timer correction, the updated
  driver passed another 60 checked 32,768-byte TCP echoes across normal detach,
  importer termination and forced socket closure. The normal run lasted 45 seconds
  and returned its budget to zero. These are functional checks, not throughput or
  endurance benchmarks. The stock USB/IP exporter was unchanged.
- macOS and Windows workspace release tests, Clippy with warnings denied and
  formatting passed; Windows documentation with warnings denied passed. The
  earlier Linux ARM64 workspace release run used Apple's `container` 1.7.1
  successfully, with no compatibility/setup/execution issues. The later retirement
  changes are Windows-specific. Native tests remain opt-in; CI runs portable
  workspace checks, not kernel-driver installation.
- The kernel build checked all 58 original-driver stack frames against the 4 KiB
  per-function cap. The signed package passed its development signature check.
  The tested `KnightBusUde.sys` SHA-256 is
  `92f23170829ce4a421e7e58c3e34b4047faa40be727ab6b6f06a2654106a3ba5`.
  This is developer signing, not production release approval.

Evidence is retained outside the repository in Mac `/tmp/knightbus-m7-finish` and
Windows `C:\Users\billy\KnightBusLab\m7-next`. Key artifacts are
`event-{native-final,held-native,reset-final,slots,workspace-release,workspace-clippy,workspace-doc}.log`,
`event-{held,retired,final}-pool.txt`, the signed package/installation records,
Mac `event-{held-long,heldshort,composite,ncm-normal,ncm-owner-exit,ncm-link-loss}.log`,
and the matching NCM packet captures. Earlier failed stock-fixture traces remain
available beside them. The durable held-client and endpoint-reset regressions
live in the existing native test suite; temporary diagnostic scripts stay outside
the repository.

Final scoped rollback removed the hash-verified WinUSB fixture and KnightBus OEM
packages. Windows had zero present test devices/controllers, importer processes,
reserved slot events or KnightBus packages. The last post-regression debugger
snapshot again showed zero `Knig`-tagged pool bytes. Linux's stock NCM module was
restored, the temporary `dummy_hcd` unloaded, and test gadgets, listeners, firewall
rules, trace probes and fixture processes were absent. Both VMs remained running;
Windows, Linux and Proxmox SSH worked, and no recovery filesystem remained mounted.
No commits were made. Temporary evidence and signed build artifacts were retained
in the lab directories for review.

## Review corrections (2026-11-13)

Three review findings were addressed without changing the agreed M7 scope:

- **Stalled writes:** an outbound frame now retains its admission timestamp.
  Every flush checks the absolute timeout before writing; partial progress and
  `WouldBlock` cannot renew it. Expiration shuts down the stream, releases the
  output buffer and drives normal native cleanup. Tests cover expiration before
  the first flush and a nonreading loopback peer. The admission test fails against
  the prior implementation, which incorrectly returned success after expiration.
- **Name squatting:** Microsoft's [object-name rules](https://learn.microsoft.com/en-us/windows/win32/sync/object-names)
  confirm that global event and mutex names share a namespace. An event ACL cannot
  protect an uncreated name. Reservations now exclusively create notification
  events under the OS-owned `\Device\KnightBus.Controller.N` path using the
  documented user-mode [NtCreateEvent interface](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-zwcreateevent).
  The Windows test removed privileges and disabled the Administrators SID:
  protected creation returned `STATUS_ACCESS_DENIED`, while creation of mutexes
  at all 64 old global names succeeded. Reverting to elevation still allowed all
  64 protected slots. The kernel and uninstaller use the same protected names.
  The readiness IOCTL changed to `0x22e42c` to reject mixed namespace contracts.
- **Endpoint starvation:** kernel FETCH and userspace mailbox delivery now rotate
  eligible slots. The worker drains a bounded batch, at most one request per
  eligible pipe, before sleeping. It retains cancellation, setting and retirement
  masks. The continuously ready two-pipe test produced **100/0** deliveries with
  the prior backend and **50/50** after the fix, and verifies that both initially
  ready pipes are fetched within one polling turn.

Final verification passed workspace release tests on macOS, Windows and Linux
ARM64 through Apple's `container`; macOS/Windows Clippy and formatting; both
Windows reservation tests; all three native ownership/fault/held-client tests;
WinUSB stall/reset recovery; and stock Linux composite interop with 120 checked
native COM round trips. No Apple `container` issues occurred. The kernel build
passed its 58-function stack-frame check and development package verification.
The new signed SYS SHA-256 is
`31c9912535a723d0912867bc5dfe9f972d289507ba33abde80bbc9c0a24261d6`.
The uninstaller also rejected an occupied protected slot before successful scoped
rollback. The post-test debugger snapshot again showed zero driver-tagged pool
bytes; the temporary Linux gadget/controller was removed. Review evidence uses
`review-*` names in the existing lab directories;
no temporary lab scripts were added to the repository.

## Cancellation, scheduling and copying follow-up (2026-11-13)

- Pending UNLINKs now get the next free output slot before new SUBMITs. An
  already partially written frame still finishes first. The regression with a
  continuously ready producer fails with the old ordering and passes with the
  fix; the five existing cancellation/retirement race cases also pass.
- Active worker batches yield without a timed sleep. Idle workers wait on a
  condition variable that completions and shutdown wake, with a shared-lock
  predicate to prevent lost wakeups. Native request arrival still requires the
  bounded 1 ms idle poll because the current driver ABI has no notification.
  A synthetic 256-completion loop on the Mac took about 320 ms with the old
  unconditional sleep and 2.9 ms with the new worker (1.4 ms on Windows). These
  measure worker overhead, not USB/network throughput; CI does not assert a
  timing threshold. Idle polling, shutdown and credit release are checked too.
- SUBMIT encoding borrows the retained native transfer's payload and ISO layout
  through the existing validator, then copies into one admitted wire buffer.
  It no longer allocates an intermediate OUT payload. Golden bytes, invalid
  inputs and allocation admission are covered; a 64 KiB OUT test succeeds with
  room only for the owner, input scratch and final frame, preserving native
  identity and ownership. This does not claim end-to-end zero-copy I/O.
- Kernel FETCH initializes only the returned header and OUT payload. Idle
  FETCH returns zero bytes; unused buffer capacity is never exposed. STATS
  initializes every returned field without clearing unrelated capacity.
- The uninstall finding described the older namespace implementation: removal
  already reserves all 64 protected `\Device\KnightBus.Controller.N` events.
  On Windows, actual package removal was rejected while a quarantined HID
  controller was absent from PnP inventory but still held by an open client.
  After that client closed, scoped package removal succeeded.

Workspace release tests passed on macOS, Windows and Linux ARM64 using Apple's
`container`, with no container issues. macOS/Windows workspace formatting and
Clippy passed, as did macOS documentation checks. Kernel formatting was checked
on the Mac; the Windows kernel toolchain lacks rustfmt. The rebuilt driver passed
the 58-function stack check, development signature verification, all three native
ownership tests, 50 varying HID reports with held-client quarantine, stock Linux
composite interop (120 COM round trips), and WinUSB stall/reset recovery.
The tested signed SYS SHA-256 is
`64ec383627c776fa5e3c51643447641258b4a6f78ee6981391a74fe881fb7a5b`.

Evidence remains outside the repository in `/tmp/knightbus-m7-review2` and the
existing Windows lab directory. Scoped cleanup removed the test driver packages
and temporary Linux gadget/controller; no importer processes or present test
nodes remained. Physical-device and sleep/wake qualification remain deferred
under the agreed scope.
