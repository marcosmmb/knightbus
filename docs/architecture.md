# Architecture

KnightBus carries USB requests between a virtual device on the importing computer
and a physical device on the exporting computer. Applications use their normal
USB drivers; KnightBus translates between native USB operations and USB/IP.

## Data flow

```mermaid
flowchart TB
    subgraph Importing computer
        App[Application and USB class driver] <--> Virtual[Native virtual USB adapter]
        Virtual <--> IW[Shared importer worker]
        IW <--> IT[USB/IP transport]
    end
    subgraph Exporting computer
        ET[USB/IP transport] <--> EW[Engine and shared exporter worker]
        EW <--> Physical[Native physical USB adapter]
        Physical <--> Device[USB device]
    end
    IT <-->|Mutually authenticated TLS 1.3| ET
```

Connection setup discovers the remote device, validates its descriptors and
capabilities, and publishes it through the importer's native USB stack. The
exporter checks access and acquires the physical device before confirming import.

For each transfer, the importer reads a native request and assigns its USB/IP
sequence. The exporter submits it to the physical device and returns the result.
The importer matches that result to the original native request and completes it.
Cancellation follows the same ownership rules as completion.

## Code map

Paths below are relative to the repository root. Crates live under `crates/`.

| Component | Owns |
| --- | --- |
| [`knightbus-usb`](../crates/knightbus-usb/src/lib.rs) | USB constants, setup packets and allocation-free descriptor validation; usable without the Rust standard library |
| [`knightbus-usbip`](../crates/knightbus-usbip/src/lib.rs) | USB/IP wire encoding, decoding and validation, with explicit allocation admission |
| [`knightbus-core`](../crates/knightbus-core/src/lib.rs) | Access policy, sessions, memory/request budgets, descriptor inventory and device workers |
| [`knightbus-transport`](../crates/knightbus-transport/src/lib.rs) | TLS identities, control liveness, USB/IP forwarding, deadlines and backpressure |
| [`knightbus-ipc`](../crates/knightbus-ipc/src/lib.rs) | OS-authenticated local management, protected host storage and the core control framing |
| [`exporters/knightbus-macos-exporter`](../crates/exporters/knightbus-macos-exporter/src/lib.rs) | IOUSBHost discovery, capture, transfers and native callbacks |
| [`importers/knightbus-windows-importer`](../crates/importers/knightbus-windows-importer/src/lib.rs) | Virtual controller ownership, driver channel encoding and Windows notifications |

The core has no sockets or OS USB bindings. The transport uses the core and wire
codec. Native adapters implement core traits; the shared crates do not depend on
an OS adapter. The upstream Windows driver ships as a separate signed binary dependency.

## Shared workers and native ownership

The core's exporter worker preserves endpoint order, serializes configuration
changes, and tracks cancellation and retirement. Its importer worker correlates
native tokens with wire requests, manages endpoint credits, and tracks selected
interface settings. The transport drives these workers and forwards messages.

Native adapters own handles, OS buffers and callbacks. They report capabilities
before device publication and implement required notifications. Normal transfer
work wakes on events; deadlines and failed cleanup can still require timed checks.

Memory and request credits are reserved before allocation or submission and stay
with their owners. Bounded queues push back on producers. A cancellation request
or timeout does not prove that an OS callback has stopped: storage remains alive
until native retirement is confirmed. Failed cleanup retains affected resources
instead of freeing memory the OS may still access.

## Native adapters

On macOS, the exporter uses IOUSBHost to access the physical device. On Windows,
the importer opens the upstream usbip-win UDE controller. An attachment owns one
handle. The adapter translates its little-endian USB/IP request frames into the
shared worker contract, preserving native sequence IDs, cancellation and replies.
The encrypted network connection and access policy remain entirely in KnightBus.
Shutdown targets only the device associated with that handle.

The importer accepts one configuration, descriptor trees up to 4 KiB, transfers
up to 1 MiB, and up to 1,024 ISO packets. Bulk and interrupt endpoints have one
forwarded request per endpoint; ISO may use spare slots. Requests awaiting a slot
have a separate bounded queue. Exhaustion terminates the attachment rather than
growing the queue indefinitely. These budgets cover KnightBus userspace, not the
upstream kernel driver’s allocations.

The pinned package cannot accept BOS descriptors, so the Windows path currently
excludes SuperSpeed. It also infers speed from bcdUSB. KnightBus adapts only the
virtual descriptor revision to the negotiated link speed, including forwarded
device-descriptor replies; the physical descriptors and device identity stay intact. Remote wake, bulk streams and scheduled ISO are not
supported. ISO uses ASAP scheduling and preserves packet lengths and errors.
Frame scheduling is handled by the upstream driver.

A clean detach issues the upstream handle-scoped shutdown command and drains
pending Windows I/O before releasing buffers. If retirement cannot be confirmed,
the session is quarantined. The upstream driver does not automatically detach a
child on process death; after a daemon crash a Windows restart may be required.

Core also owns configuration changes, alternate-setting changes and halt recovery.
It validates each target and drains only the affected requests. Native adapters
implement explicit device/interface operations; each interface owns its own pipe
handles, so unrelated interfaces can keep their outstanding transfers.

To add an adapter, implement the core's native driver trait and reuse the workers
and transport. See [Contributing](../CONTRIBUTING.md#adding-an-os-adapter).

## Management path

```mermaid
flowchart TB
    Host[Host application] --> Direct[ControlConnection]
    Host --> Local[Unix socket / Windows named pipe]
    Local --> Framed[OS authority and versioned framing]
    Framed --> Direct
    Direct --> Engine[Core engine]
    Engine --> Policy[Grants, leases, attach / detach, events]
```

Management operations use the same engine through direct calls or OS-authenticated
IPC. Unix sockets check peer credentials inside a private directory; Windows
pipes reject remote clients and use an explicit owner/SYSTEM ACL. The C ABI and
installed daemon are separate hosting layers; this crate supplies their shared
control contract and native IPC boundary.

USB forwarding uses Mio readiness alongside native USB wakeups. Local management
uses Tokio for asynchronous Unix sockets and Windows named pipes with I/O
deadlines; its runtime does not schedule USB transfers.

Remote peers enroll certificates explicitly. TLS verifies the certificate chain,
exact paired identity and channel type, with resumption and early data disabled.
Pairing does not grant access: the engine filters device discovery and checks every
import against exporter grants. Administrators can inspect or revoke those grants.

```mermaid
flowchart TB
    Identity[Confirmed peer certificate] --> Control[Authenticated control link]
    Control --> Ticket[Short-lived, one-use import ticket]
    Ticket --> Data[Authenticated USB/IP connection]
    Data --> Grants[Exporter grants and native acquisition]
    Control --> Events[Exporter events and importer heartbeats]
    Events --> Lease[Expiring device lease]
    Grants --> Lease
    Lease --> Cleanup[Disconnect or revoke: drain native ownership]
```

Control and USB/IP use separate TLS connections bound to the same identity and a
one-use ticket. Heartbeats and bounded change notifications never enter USB/IP
frames. Losing control liveness drains the associated device even if its USB
socket stays open. Standard USB/IP compatibility connections use filtered
DEVLIST requests to refresh discovery instead of the separate event channel.

Connection counts, handshakes, management frames, event queues and transfer
storage have independent limits. Authentication and management limits do not
throttle normal USB transfers. A restart uses a fresh host epoch and requires
new authentication and acquisition; it never resumes stale native handles.

## Pairing and sharing

Each runner accepts `identity STATE_DIR` to create protected credentials and
`fingerprint STATE_DIR` to display its public identity. Exchange the generated
`certificate.der` files through a trusted channel, confirm the full fingerprints,
and enroll each side with `pair STATE_DIR CERTIFICATE.der FINGERPRINT`.
Pairing alone grants no device access. Keep the state directory private; never
copy `identity.bin` to another computer.

The macOS exporter accepts:

```text
serve REGISTRY_ID USB_ADDRESS:PORT CONTROL_ADDRESS:PORT STATE_DIR IMPORTER_FINGERPRINT --capture MANAGEMENT_SOCKET [--storage-unmounted]
```

This explicitly grants the selected importer access to that physical device.
Capture requires OS permission; storage volumes must first be cleanly unmounted.
The management socket belongs in a separate private directory owned by the exporter
account, outside `STATE_DIR`. The credentials directory contains only identity and
peer records.
The Windows importer accepts:

```text
--tls USB_ADDRESS:PORT CONTROL_ADDRESS:PORT STATE_DIR EXPORTER_FINGERPRINT BUS_ID SECONDS
```

Local management uses `manage SOCKET list`, `permissions DEVICE_ID GENERATION`,
`grant DEVICE_ID GENERATION PEER`, `deny DEVICE_ID GENERATION PEER`, or `shutdown`
after the `manage SOCKET` prefix. Device IDs are hexadecimal. Local management is
restricted to the host account. `unpair STATE_DIR FINGERPRINT` removes trust;
running exporters reload it and drain affected devices. Rotating an identity
requires new state and explicit re-pairing. Disconnections never silently reattach.

Traffic is encrypted between paired hosts, but a permitted importer controls the
USB device and can read its data. Ordinary logs contain status and metadata,
not USB payloads or private keys.

## Application hosting

`knightbus-sdk` composes the native backends and existing core/transport hosts.
`knightbusd` adds an authenticated local listener and process shutdown handling;
`knightbus-ffi` adds opaque C handles, explicit response ownership and panic
containment. Neither adapter implements USB forwarding. Service management clients
can disconnect while native forwarding continues. Embedded owners retain the
host until shutdown and confirmed cleanup. See the [control API](control-api.md)
and [standalone commands](standalone.md).

The optional `knightbus-sdk/mdns` feature supplies untrusted address hints only.
It never changes identity enrollment or device authorization. User configuration
selects one native role; starting an importer does not automatically attach.
