# macOS physical USB exporter (M8 development backend)

Explicit capture is available for development. The owner deferred investigation
of the earlier macOS 27.2.1 kernel panic while completing M8; this is not a claim
that the panic is fixed or that the backend is approved for production release.
See the [hardware report](../../../docs/milestone8-validation.md) for measured
scope and the [optional crash recorder](../../../docs/macos-crash-lab.md).

Build the Rust crate with Apple's command-line tools and Rust 1.85 or later:

```sh
cargo build -p knightbus-macos-exporter --locked
cargo test -p knightbus-macos-exporter --locked --offline
```

Only ordinary OS bindings are dependencies: objc2, block2, Foundation, IOKit and
IOUSBHost, with their general-purpose supporting crates. The Cargo lockfile pins
them. There is no forwarding dependency, OS-required native shim or DriverKit
extension. Non-macOS builds include the portable worker and parser tests; the
diagnostic executable reports that physical access needs macOS.

`knightbus-macos-exporter list` enumerates without claiming interfaces. `inspect
REGISTRY_ID` opens only the device for standard descriptor reads and closes it.
Neither command grants remote access. `access LOCATION` is an explicit ordinary
interface-open diagnostic for the selected Logitech device. Access failure is
reported with the underlying IOReturn when available. No command escalates itself.
Capture requires an explicit `--capture` argument in the diagnostic CLI, or
`capture: true` in the library after the host has authorized the selected physical
generation. There is no automatic elevation or capture during enumeration. The
lab journal is optional; once requested, failed recording refuses new operations
and the recorded target restriction remains in force. Generic library capture is
not restricted to a VID/PID; the destructive qualification fixtures are restricted
to the lab joystick so they cannot accidentally exercise a storage device.

For local development, build and run from the checkout (no installation needed):

```sh
cargo build -p knightbus-macos-exporter --release --locked
target/release/knightbus-macos-exporter list
# Use the current registry selector from list; this only reads descriptors.
target/release/knightbus-macos-exporter inspect 0xCURRENT_REGISTRY_ID
# Use the joystick's location from list; one bounded shared-engine cycle.
sudo target/release/knightbus-macos-exporter qualify 02120000 1 --capture
```

To install only the diagnostic executable in your development account:
`cargo install --path crates/exporters/knightbus-macos-exporter --locked`.
This does not install a service, privileged helper, extension, signing identity,
or entitlement. Root is a measured development access requirement for the tested
OS-owned HID interface. Do not run a desktop GUI as root. An ordinary embedded
application must report `Denied` for unavailable capture; a separately authorized
privileged host must own physical USB access. SIP remains enabled.

The library exposes `native::enumerate`, `native::inspect`, `native::Native` and
the bounded `Backend` implementing `knightbus_core::ExporterBackend`. Construct
the backend with `Engine::with_backend_factory` so it shares aggregate memory
admission with the engine. Construct `Native` inside the worker factory, not on
the host/UI thread. The engine must authenticate the importer, register the
physical generation, grant it explicitly and acquire a lease before submission.
The factory is invoked only after that acquisition begins. Both host adapters
use this same engine/backend contract.

The in-process fixture routes authorization/attach through `InProcessControl`
and uses the same library backend when hosted by a temporary root launchd job.
Both root contexts passed; ordinary-user capture returned `Denied`. The
finalization harness also passed owner death/reclaim and local HID input recovery.
A launchd host must keep its binary/state outside privacy-protected user document
directories; the lab uses root-only temporary storage under `/private/var/tmp`.
This is M8 host-loadability testing, not M12's full IPC/Embedded parity suite,
foreign ABI, installer or performance qualification. The historical panic batch
stays disabled; use `tests/crash_lab.py --help` for retained evidence and bounded
trials. No persistent launch daemon is installed by this crate or its tests.

Release/reset may cause a fresh registry identity. The host must rediscover and
reauthorize it; stale generations cannot reopen a replacement device. Drop asks
the worker to drain without blocking the host; native storage and device ownership
remain retained until callbacks retire. A cleanup deadline is not permission to
free native buffers. API/OS internal allocation overhead is not observable, while
request counts and owned data/metadata allocations are explicitly bounded.

Bulk/interrupt/control and ASAP ISO code exists. Instrumented joystick tests now
cover shared-engine capture/release, descriptors, configuration/alternate
reselection, interrupt input, STALL/recovery, cancellation and explicit reset.
Active and idle removal, captured-owner death/reclaim and local input recovery
also passed. The original panic remains unresolved; bulk/ISO and other device classes remain
unqualified. See the report for exact limits and archived evidence identities.


Worker admission distinguishes `SubmitError::Busy` (retry after a notification)
from `SubmitError::Failed`, including terminal scratch-budget exhaustion (`ENOMEM`).
Requests retain admission order per endpoint/direction, independent of USB/IP
sequence-number values; topology barriers also wait for earlier queued requests.
The worker redispatches after retiring completions and sleeps on a latched waker
when idle or awaiting native I/O. Native control/pipe/ISO completions and removal
callbacks wake it. Only failed abort/release APIs use the cleanup retry timer.
Custom `Driver` implementations must install and honor `set_waker`; cancellation
returns an error when a timed API retry is needed. Buffers still retire only after
terminal completion. Named wire/status constants and bounds live in
`src/constants.rs`; Apple SDK constants are used where the bindings expose them.
