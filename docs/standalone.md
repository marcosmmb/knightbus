# Standalone operation

Build the applications and library with `cargo build --workspace --release --locked`.
The main programs are `knightbusd`, the forwarding daemon, and `knightbus`, the
CLI for pairing, exporter and importer management, inspection, and diagnostics.
The host determines which operations its backend supports. The library is
`libknightbus.dylib`, `libknightbus.so`, or `knightbus.dll`, with a static library
also available.

The **exporter** runs on the computer physically hosting a USB device. The
**importer** runs on the computer that will use it through its native USB stack.
The workflow below applies to either role wherever a suitable backend is available.
Backend availability, device capabilities and installation requirements depend on
the operating system and build; see [native adapters](architecture.md#native-adapters)
for implementation-specific details.

The daemon runs in the foreground. Its local endpoint and credentials belong to
its OS account. Normal same-account management requires no elevation. Both hosting
modes require the native USB permissions and any drivers or extensions needed by
the selected backend. Running the daemon under a different account does not
automatically grant other accounts access to its endpoint; use a supported
account and backend permission arrangement.

## Paths and local endpoints

Commands below use uppercase placeholders such as `EXPORTER_STATE`,
`IMPORTER_CONFIG` and `IMPORTER_ENDPOINT`. Replace them with paths or endpoints
on the computer running that command, quoting them as required by your shell.
Each computer needs its own private identity directory and local control endpoint.

Local endpoint syntax depends on the operating system, independently of the role:

| Platform | Example local control endpoint | Setup |
| --- | --- | --- |
| Unix | `/absolute/private/control/socket` | Use an absolute socket path in an owner-only directory, separate from the identity store. |
| Windows | `\\.\pipe\knightbus-control` | Choose a local named-pipe name; the daemon creates it with restricted access. |

For example, create the Unix endpoint directory with
`mkdir -m 700 /absolute/private/control`. Keep configuration files and identity
stores private on every platform.

## Select a device and pair the computers

On the exporter computer, enumerate without capturing any interfaces:

```text
knightbus local-list
knightbus local-inspect PHYSICAL_GENERATION
```

Use the physical generation identifier reported by `local-list` for inspection.
Inspection prints configurations, interface alternates, endpoint types, packet
sizes and intervals. `claimability=untested` means descriptor access succeeded;
it does not mean an OS-owned interface can be captured. `NotFound` means that
exact physical generation is absent. A descriptor access or later native claim
failure is distinct from absence. Select by the displayed generation, never by
VID/PID alone. Do not capture a keyboard, mounted storage, or another device in
active use. Storage capture additionally requires cleanly unmounting every volume
and setting `storage_unmounted=true`.

Create a separate identity directory on each machine:

```text
knightbus identity EXPORTER_STATE
knightbus identity IMPORTER_STATE
```

Each command prints its public certificate fingerprint. Exchange only the
`certificate.der` files from those directories; keep private keys on their owning
machine. Confirm the fingerprint through a trusted channel, then enroll each
side explicitly:

```text
knightbus pair EXPORTER_STATE importer.der IMPORTER_FINGERPRINT
knightbus pair IMPORTER_STATE exporter.der EXPORTER_FINGERPRINT
```

Pairing grants no device access. `fingerprint STATE` displays the local identity;
`unpair STATE FINGERPRINT` revokes a peer. Running hosts reload trust changes;
revocation drains affected forwarding. Replacing the local identity requires a
restart. Discovery and imported data require mutually authenticated TLS; these
applications do not offer implicit plaintext fallback.

## Start the exporter and grant sharing

Use [exporter.conf](../examples/exporter.conf) as a configuration template. Fill
in the real addresses, identity directory and device selection required by the
exporter backend, and protect the configuration from other users. `capture=true`
explicitly permits claiming the selected device when an authorized import arrives.
Startup grants are optional; no grants means no remote visibility or access.
After release or removal, refresh the device selection and follow the backend's
recapture requirements; do not treat an old selection as a new physical device.

Start the exporter host with its configuration and local control endpoint:

```text
knightbusd EXPORTER_CONFIG EXPORTER_ENDPOINT
```

In another terminal on the exporter computer:

```text
knightbus manage EXPORTER_ENDPOINT info
knightbus manage EXPORTER_ENDPOINT list
knightbus manage EXPORTER_ENDPOINT inspect DEVICE_ID GENERATION
knightbus manage EXPORTER_ENDPOINT share DEVICE_ID GENERATION IMPORTER_FINGERPRINT
knightbus manage EXPORTER_ENDPOINT permissions DEVICE_ID GENERATION
```

IDs accept decimal or explicit `0x` hexadecimal. Generations and session tokens
are full 64-bit integers. `deny ID GENERATION PEER` revokes one importer;
`unshare ID GENERATION` revokes every grant and drains affected leases. Runtime
grant changes are ephemeral. Restart applies only the explicit `grant=` entries
in the configuration. Editing a configuration does not silently reload consent.

## Discover, attach, inspect and detach

Use [importer.conf](../examples/importer.conf) as a configuration template. Set
its local identity directory and the paired exporter's addresses and fingerprint,
and apply any settings required by the importer backend. Starting an importer
never automatically attaches a device.

On the importer computer:

```text
knightbus discover IMPORTER_CONFIG
knightbusd IMPORTER_CONFIG IMPORTER_ENDPOINT
```

From another terminal on that computer:

```text
knightbus manage IMPORTER_ENDPOINT list
knightbus manage IMPORTER_ENDPOINT attach DEVICE_ID GENERATION
knightbus manage IMPORTER_ENDPOINT status EPOCH SERIAL
knightbus manage IMPORTER_ENDPOINT sessions
knightbus manage IMPORTER_ENDPOINT inspect DEVICE_ID GENERATION
knightbus manage IMPORTER_ENDPOINT detach EPOCH SERIAL
```

Use the importer host's latest `list` generation in `attach`. Its selection token
belongs to that local discovery snapshot: standard USB/IP does not transmit the
exporter's generation token. A new list invalidates previous selections. The
exporter independently rechecks its current grant and physical inventory on
import. Descriptor inspection on the importer becomes available after successful
import; unauthenticated discovery never reads descriptors. Concurrent attachment
limits depend on the backend and its resource budget. No automatic reconnect or
replay occurs.

Attach returns `Acquiring`, not proof of native publication. Wait for `Active`.
Detach also cancels a pending attach; wait for `Closed` before reuse. `Draining`
means native requests remain outstanding; `Quarantined` means cleanup exceeded
its deadline and resources are still owned. Never interpret either as release.
`forget EPOCH SERIAL` discards only a closed session record.

## Events, errors and shutdown

`manage ENDPOINT watch SECONDS` subscribes to bounded control events. A `Resync`
event means a slow reader must refresh inventory and sessions. Closing the CLI
retires its subscriptions without ending forwarding. The daemon directly owns
networking, USB requests, and native attachment. There is no per-transfer
application IPC request.

`manage ENDPOINT shutdown` acknowledges the request then drains native work.
Ctrl+C and termination signals also request a graceful drain. Do not force-kill
a quarantined host and assume its native resources were released. After a crash,
check that the previous process is gone and native resources retired before
removing a stale local socket and restarting. Windows named pipes disappear with
their owning process. A restart has a new epoch; never replay old session IDs.

The daemon writes structured `key=value` records to stderr. Its metadata audit
identifies authentication, permissions, connections, disconnect causes and audit
overflow. CLI errors include a stable category and suggested action. `Denied`
can mean policy, OS endpoint permissions or native USB access; inspect the phase
and session status. `Deadline` is not proof that an earlier mutation did not
execute. Reconnect and inspect before retrying. `Version` requires matching
control versions. Native platform errors cannot be fixed by changing hosting mode.

An optional diagnostic bundle contains host version/epoch and session metadata:

```text
knightbus manage EXPORTER_ENDPOINT diagnostics DIAGNOSTICS_FILE
```

The output must not exist. It includes OS/architecture, device/session identifiers
and sanitized error categories, but no configuration files, certificates, keys,
tickets, raw USB payloads or serial-string descriptors. Store it privately; it can
still identify a session. Collection is paced to respect the local request limit.
The report records `complete=false` and the command exits with an error if any
query fails; a saved file alone does not mean collection succeeded. Diagnostics
do not claim a successful native cleanup.

## Optional LAN address discovery

Build with `--features knightbus-sdk/mdns` to enable DNS-SD. Set `mdns_name` in
an exporter configuration using the same concrete LAN IP for both listeners;
wildcard and loopback advertisements are rejected. Advertisements contain only
the control port, USB port and control version, not device inventory or grants.

`knightbus browse 3` returns up to 64 untrusted IPv4 address hints during
a bounded search (1–10 seconds). Select an address explicitly, verify the peer's
fingerprint independently, pair, then use authenticated discovery. No advertised
identity is accepted as a credential. Explicit IPv4/IPv6 addresses work without
mDNS. Multicast availability is OS/network dependent; advertisement failures are
reported separately and do not authorize plaintext fallback.

## Embedded applications

Use the [C header](../crates/knightbus-ffi/include/knightbus.h) and
[example](../crates/knightbus-ffi/examples/control.c), or the Rust SDK's
`Runtime::start` and `Embedded::connect`. Service clients use `Client::connect`
or `kb_service_open`. Both adapters expose the [same control contract](control-api.md).
Keep the embedded runtime alive for the whole forwarding session; its last owner
requests shutdown and waits for native cleanup. Unlike a Service client exit,
closing an embedded host ends forwarding.
