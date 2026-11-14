# Implementing a native adapter

Implement the native driver trait and compose it with the shared worker. USB/IP
framing, queues, correlation, cancellation delivery and lifecycle policy belong
to the shared crates. An OS adapter must not copy those loops.

## Boundaries

| Layer | Shared responsibility | Native responsibility |
| --- | --- | --- |
| USB metadata | Structural validation, checked topology and owned descriptor storage | Discover physical identity, read descriptors, encode the native attachment |
| Admission | Aggregate byte/request credits and declared capability checks | Declare native storage bounds; retain reservations for OS-owned storage |
| Export | Endpoint order, configuration barriers, cancellation and retirement tracking | Open/capture, report selected endpoints, submit/abort and deliver terminal callbacks |
| Import | Request sequence/token correlation, fairness, selected alternates and cancellation delivery | Publish a virtual device, fetch requests, complete tokens and report liveness |
| Scheduling | Latched notifications and bounded work per turn | Signal every native progress change after publishing state |
| Transport | Bootstrap, configuration verification, framing, backpressure and forwarding | Supply an authorized connection through the host |
| Cleanup | Stop admission, drain, deadlines and quarantine | Prove that native access has stopped before reporting retirement |

The low-level `ExporterBackend` and `ImporterBackend` traits are engine boundaries,
used by the shared workers and test doubles. A new native port implements the
smaller `exporter::Driver` or `importer::Driver` contract instead.

## Exporter

Implement `knightbus_core::exporter::Driver`. Every operation is required:

- `CAPABILITIES`: finite transfer, request, ISO, speed and descriptor bounds.
- `set_waker`: install the worker notification before native work begins.
- `endpoint`: return the endpoint currently selected by the OS.
- `submit`: accept an owned transfer or return its unchanged owner with an error.
- `cancel`: request cancellation; this does not retire the transfer.
- `completion`: return the transfer only after all native access has ended.
- `removed`: report physical removal even when no transfer is pending.
- `release`: prove that physical ownership and callbacks have retired.

Construct `exporter::Backend` inside the `Engine::new` factory. Open native objects
inside the worker factory, using the engine's budget. The factory runs after
claim authorization, on the worker thread. There are no device-class exceptions.

The shared worker validates every request against the driver's selected endpoint
before calling `submit`. It preserves admission order per endpoint and serializes
configuration/alternate-setting changes against older requests. The native driver
still performs those changes and retains the actual OS pipe objects.

`SubmitError::Busy` means retry after a native wakeup. It must not be used for
terminal allocation failure. Failed abort/release calls get bounded cleanup
retries; normal completion and idle paths park on notifications.

## Importer

Implement `knightbus_core::importer::Driver`:

- `CAPABILITIES`, `NATIVE_BYTES`, `SCRATCH_BYTES`: explicit feature and storage bounds.
- `Signal`: a charged native notification primitive with required `new`, `notify`
  and `wait`. Use `ThreadSignal` when native callbacks can invoke Rust; use an OS
  event when the kernel must signal it.
- `attach`: publish the checked descriptor set. Failure still requires cleanup.
- `fetch`: return a typed request from the admitted endpoint-slot mask.
- `complete`: complete or acknowledge cancellation of the exact native token.
- `snapshot`: report live slots and their request tokens.
- `notifications`: signal existing work, then arrivals and cancellations.
- `detach`: confirm removal and native retirement.

Use `knightbus_transport::importer::connect` for the complete pipeline. It derives
limits from the driver type, discovers and checks descriptors, verifies remote
configuration, reserves worker/native storage and supplies the same budget to the
native factory. The resulting `Forwarder` provides bounded `tick`, event-driven
`wait`, `device` and `disconnect` operations.

Native request tokens cannot be reused while outstanding. `fetch` must be
nonblocking and return only initialized OUT payload bytes; IN requests have an
empty payload. The shared worker checks request shape and constructs charged
transfer storage. Borrowed scratch expires at the next native call; a driver may
not retain its address. Native asynchronous storage needs its own retained owner.

The shared worker delivers cancellation exactly once, after handing the request
to the transport. It cannot fetch a replacement on that endpoint until the old
remote request retires. Successful control completions update selected settings
before admitting further requests.

## Ownership and wakeups

Notification methods have no silent default. Signals must latch a wake that
arrives before a wait. Native callbacks publish state before waking and must not
acquire a lock already held by the worker during a native call. Registration must
also signal existing work. Traits require these operations; tests must verify
that an implementation honors their timing and ownership semantics.

A cancellation request, timeout, closed socket or dropped Rust host is not proof
of native retirement. Native owners retain buffers and credits until callbacks
stop. Failed retirement quarantines ownership; it must not enable a new attachment.
The shared importer also rejects foreign transfer budgets and permanently refuses
reuse after a clock regression.

Trust boundaries remain independently checked. The Windows kernel revalidates its
private channel messages and native descriptor policy using
`knightbus-windows-protocol`, which depends on the shared `knightbus-usb` parser.
Moving validation into shared code does not remove kernel validation.

## Tests and current limits

Run `cargo test --workspace --all-targets --locked --offline`. Native lab tests
are retained separately under the gitignored `lab/` directory; use its standalone
package instructions when testing a new adapter. The portable regressions live in `knightbus-core/tests/exporter.rs`,
`knightbus-core/tests/importer.rs`, the core engine tests and `knightbus-transport`.
They cover ordering, topology barriers, admission failure, cancellation before
delivery, duplicate completion, wake races, idle parking, retirement and cleanup
failure. Native tests must additionally prove callback lifetime, notification
registration, physical/virtual removal and OS handle cleanup.

Capabilities describe implemented behavior, not a promise that every USB feature
exists. The current shared importer handles control, bulk and interrupt traffic,
one selected configuration, up to 64 interface/alternate pairs and one outstanding
request per endpoint address (31 slots including EP0). It rejects ISO. Windows
additionally limits descriptor trees to 4096 bytes and rejects remote wake;
its native stream support remains unchanged. The exporter supports a broader
physical descriptor/ISO model, subject to its native driver and qualification.
Adding ISO, streams or new configuration behavior requires extending the shared
contract and its tests, then implementing the native operations. Do not implement
such features as a separate OS forwarding path.
