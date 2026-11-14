# ADR: M7 Windows importer implementation

2026-11-11. Status: development implementation; production enablement gated.
This refines the [Rust-first direction](windows-rust-driver.md), without waiving
its framework-retirement or release requirements.

## Boundary and ownership

The host supplies an authorized connection and a fresh session ID. `Wire` uses the
portable USB/IP codec; `Forwarder` correlates remote replies with the exact native
request and handles unlink races. Absolute deadlines cover frame progress,
attach, control requests, cancellation and cleanup. Idle bulk/interrupt reads wait
for native cancellation; they do not expire merely because no data is available.
Each output frame's deadline starts at admission and is never renewed by short
writes or `WouldBlock`. Expiration closes the connection and starts native drain.
Pending cancellation messages take the next free output slot before new SUBMITs;
an already admitted partial frame finishes first.
Hosts retain responsibility for session leases and aggregate admission limits. Both service and
embedded callers use this same library. The development CLI explicitly opts into
raw, unauthenticated USB/IP; it is not a production service or TLS implementation.

`Backend` owns one attachment and a dedicated synchronous native-I/O worker.
Engine polling never executes a blocking IOCTL. Fixed mailboxes carry transfers,
completions and cancellation intents; shared `Budget` credits cover live payloads,
scratch and metadata. The worker alone owns the controller file. Dropping the host
signals drain; the worker retains its storage until native cleanup returns.
Native OUT data is borrowed directly by the shared codec into one charged output
frame. The original transfer stays owned and charged until remote/native retirement;
there is no temporary payload Vec between that owner and the frame.
Failed cleanup conservatively retains the native reservation instead of claiming
retirement. Thread stacks, socket buffers and framework-owned allocations require
separate platform limits/measurement; `Budget` is not total process/kernel RSS.

The driver accepts only bounded, versioned, pointer-free IOCTLs. It independently
validates raw device/configuration descriptors and publishes an actual USB child
through UDE. Windows class drivers choose the binding; no VHF, joystick-specific
translation or third-party USB/IP driver is involved. Remote bytes never become
native pointers. USB/IP framing, networking, TLS, identities and device grants stay
in userspace. The private device interface permits SYSTEM/administrators only;
a controller's owner file exclusively controls its child and tokens.

Each endpoint has a sequential framework queue and one active copied payload
slot. Independent endpoints can be outstanding concurrently. There are 31 slots
and at most 62 simultaneously retiring/current owned endpoint queues. This bounds
the private channel, not the Windows USB stack's upstream queued requests.
Kernel FETCH and userspace mailbox delivery use round-robin selection. The worker
fetches at most one request per eligible pipe in a bounded batch;
cancellation, active-setting masks and retirement still gate each pipe.
After progress it yields and continues without a timed delay. When idle it waits
on a condition variable, woken by a completion or shutdown under the mailbox's
mutex predicate. Native request arrival still requires a 1 ms idle polling timeout
because the current private ABI has no arrival notification. Host tick scheduling
also remains independent of this worker policy.

A spin lock arbitrates slot ownership between cancellation and userspace replies.
Foreign forwarded WDF requests receive our context before admission or marking
cancelable. If allocation fails, the exclusively owned, noncancelable IRP carries
an allocation-free failure-list entry in its documented DriverContext fields;
those fields are cleared before completion. URBs complete on a separate DPC at
DISPATCH_LEVEL, including allocation failure and cancellation; native request buffers
are accessed only while the driver owns the request. A passive gate serializes
attach/detach and owner-file cleanup. UDE owns endpoint queue start/purge state.
These choices follow [Microsoft's UDE requirements](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/writing-a-ude-client-driver).

`PlugOutAndDelete` consumes the child handle. Cleanup stops admission, retires
owned requests, drains accepted configuration forwards, calls that DDI once, then
waits finitely for owned endpoint queues within the same deadline.
No callback accesses a consumed handle. A timeout quarantines the controller.
Owner exit uses the same drain path. Independent controller instances can be
selected explicitly by native instance ID; a two-controller native fixture test passes isolation and owner-exit cleanup.
The normal installer stages the driver package. The common Rust backend creates
and owns a controller for each attachment; explicit preinstalled-controller access
is retained only for diagnostic tests. This does not qualify all multi-device
hardware/power behavior.

## Deliberate limits and open work

- One configuration, up to 64 interface/alternate pairs, 30 nondefault endpoint
  addresses, 4 KiB configuration/BOS descriptors and 1 MiB native transfers.
  Fixed payload storage is charged as 31 MiB per controller; host admission
  budgets must include that reservation before native creation.
- Low/full/high/SuperSpeed/SuperSpeedPlus metadata is validated. UDE publishes
  the latter two as SuperSpeed; static streams remain unsupported.
  Control, bulk and interrupt routing
  exists; full-speed CDC and NCM fixtures have native application evidence, with the
  reliability limitations recorded in the findings.
- Import selects and reads back the sole supported configuration before native
  publication; this also resets alternate settings to zero. Failure prevents attach.
  Later SET_CONFIGURATION and SET_INTERFACE requests are intercepted by an original
  lower filter on that controller's virtual root hub. Native provisioning validates
  the exact parent/hardware ID and preserves other per-device filter entries; no
  physical hub or class-wide filter is modified. A bounded registry associates
  each filter with a referenced controller. Attach requires that filter to be ready.
  Validated actual setup packets use the ordinary EP0 transport, then the original
  request proceeds to UDE only after remote success. Endpoint notifications retire
  local queues; their stale metadata never determines remote alternate selection.
  Periodic transfer intervals follow the selected alternate descriptor. Endpoint
  halt reset forwards raw and typed CLEAR_FEATURE requests through that same
  filter; a callback alone missed the typed path. USB/IP provides no standalone physical bus-reset
  command, so that operation fails explicitly.
- ISO/SuperSpeed/multiple configurations, remote wake, DMA/shared-memory zero copy, native
  transfer-flag fidelity and full reset/power behavior require further work.
  Large native URBs fail rather than being silently truncated.
- On the persistent-controller diagnostic path, total UDE control queues grow per
  attachment: fifty additional cycles
  showed no plateau, with 1,088 driver-tagged nonpaged bytes retained per cycle
  in the measured final ten. Removing the controller reclaimed that pool tag.
  NCM's separate owned-EP0 retirement failure was corrected by acknowledging local
  endpoint releases during unplug instead of forwarding stale setting metadata.
  Neither a timeout nor a successful private DETACH means the entire framework
  passed resource testing.
  A [paired C++/Rust experiment](../research/windows-rust-driver-feasibility.md#c-comparison-of-control-queue-retention)
  reproduced identical control-queue growth; changing language alone is not a
  demonstrated fix. Retention also reproduced after upgrading to Windows 11 26H2
  and in a simple-endpoint variant without dynamic release callbacks.
  The current request-context and root-hub-filter implementation passes native NCM
  TCP traffic and fault cleanup with Ubuntu's stock 6.8 exporter. The intermittent
  echo timeout was traced to the Linux NCM gadget failing to retry a busy timer
  flush; correcting that external fixture passed the controlled repeats. These short fixture runs
  do not establish general network-device reliability.

The normal CLI and common backend now use `Controller::owned()`. Native creation
runs inside the worker after memory admission, and never on the engine polling
thread. A system/administrator-only named manual-reset event reserves one of 64
fixed slots across processes. Its existence is the reservation; no thread ownership
is acquired. Events are created exclusively with `NtCreateEvent` under the OS-owned
`\Device` object directory, whose parent ACL protects vacant names from
unprivileged creation. The event's SYSTEM/Administrators-only ACL protects the
created object. Predictable names in Win32's global namespace were insufficient:
an unprivileged process could precreate even a different object type there.
The kernel opens its own handle before publishing interfaces and
retains it until the controller's final WDF destroy callback. Process exit closes
user handles, but cannot release a slot still retained by an open USB client.
Persisted controller IDs are reused, so crashes do not create an unlimited
sequence of controller records. Ordinary Windows USB child history is distinct
from those bounded controller slots.

`SwDeviceCreate` returns the actual instance identity through a native Rust
callback. Callback storage remains alive through `SwDeviceClose`, including timeout
paths. Provisioning changes only the returned controller's verified virtual hub.
When first adding its filter, it recreates the empty controller before publishing
any USB attachment. Normal cleanup retires the USB child, closes the channel and
software handle, and waits for both controller/hub absence and the kernel's
final-destruction event before releasing its slot and native credits. PnP absence
alone is insufficient: an application's open USB file can retain the controller.
The zero-payload `RETIREMENT_READY` IOCTL rejects older drivers before USB
publication. The event is opened by a validated fixed name, never by accepting a
user pointer or handle. Only the controller's WDF destroy callback signals it.
An unknown identity or missing acknowledgement quarantines the slot and native
reservation for the remainder of the process. After process death the kernel
still prevents slot reuse until the final client closes. The staged-package
uninstaller reserves all event names and refuses active/quarantined slots,
including those whose creator has exited, before deleting the verified package.

The integrated native tests cover partial attach failure, repeated normal use,
network loss, importer death, held client files, bounded slot reservations and
two-process isolation. Physical-device and sleep/wake qualification are explicitly
deferred from M7 at the owner's request; they remain requirements for broader
platform qualification.
The measured resource results and remaining platform gates are recorded in the
findings; this ownership change is not a blanket production qualification.

The kernel workspace is isolated because WDK binding generation, kernel linking,
IRQL rules, panic behavior and toolchain/profile differ from portable Rust. Its
unsafe code is confined to Windows FFI and original native request ownership;
portable protocol/core crates retain their existing safety policy.

Platform importer and exporter crates live under `crates/importers/` and
`crates/exporters/`, respectively, as members of the main Cargo workspace. Shared
libraries stay directly under `crates/`. Kernel drivers and their packaging live
under `drivers/<platform>/`; the Windows driver keeps its separate workspace and
toolchain. Create future backend directories only when they are implemented.
