# Control API and C ABI

`KnightBusControl` in `knightbus-core` is the application contract. Both adapters
use its `Command`, `Reply`, stable `Error`, session state and bounded events.
`knightbus-sdk::Embedded` calls the same host that `knightbusd` exposes through
OS-authenticated `knightbus-ipc`. `Runtime::exporter` also accepts an application
provided engine/backend without introducing a second forwarding implementation.

The current control schema is version 1; the independently versioned C ABI is 1.
`Info` returns the control version, restart epoch and monotonic milliseconds.
The SDK negotiates before exposing operations, translates relative timeout
milliseconds to the host clock, and never automatically replays failed calls.
`KnightBusControl::call` itself takes an absolute **host-clock** deadline. Do not
pass a wall-clock timestamp or another process's uptime. The C `kb_call` timeout
is relative and replaces the frame's deadline field.

The IPC client has a five-second I/O bound. Expired/ambiguous I/O invalidates its
connection; create a new connection and inspect state before retrying a mutation.
Queued deterministic services can cancel undispatched request IDs. Live endpoints
execute in order; accepted asynchronous attach is cancelled with `Detach`, followed
by events/status until retirement. No successful cancellation implies that native
buffers have already retired.

Local callers derive authority from OS credentials, never request fields. Unix
listeners require a private parent and same-UID peers; Windows pipes have a
protected owner/SYSTEM ACL and clients verify endpoint ownership. Events belong
to their connection. Another connection cannot poll a subscription even under the
same OS account. All pending subscriptions retire on disconnect; device leases
remain host-owned. Native drivers and entitlements are independent requirements.

## C ownership and threading

`kb_embedded_open` accepts UTF-8 configuration bytes. `kb_service_open` accepts a
UTF-8 local endpoint path. Both require the expected ABI version and set the output
handle to NULL on failure. They never retain input pointers. Each handle must be
used by one thread at a time; distinct handles can be used concurrently. C callers
must supply valid, aligned objects and correctly sized readable/writable buffers.
NULL and length validation cannot make an invalid C address safe.

`kb_call` accepts one bounded request frame with increasing nonzero request IDs.
A zero return means a `KbBuffer` response exists; the response itself contains the
operation result/error. A nonzero return is a validation/adapter failure and
produces no buffer. `kb_buffer_data` and `kb_buffer_len` borrow immutable bytes;
`kb_buffer_free` releases the buffer exactly once. Responses survive closing the
originating handle. All errors are owned numeric values; there are no borrowed
error strings or thread-local last-error pointers.

`kb_close` consumes a handle exactly once. For Embedded it stops and joins the
native host; for Service it closes only the management connection. NULL close/free
are no-ops. Panics at callable mutation boundaries are contained; a panicking
handle is poisoned and cannot be reused. A panic does not establish native cleanup.
Do not build this library with `panic=abort` if panic containment is required.

Subscribe/Poll/Unsubscribe provides asynchronous, bounded event delivery with no
application callbacks under engine locks. On overflow, a `Resync` event requires
new snapshots. USB reports, URBs and buffers have no representation in the control
request schema. Polling must respect the local rate limit (32 requests per second).

## Binary management schema

Use `knightbus-ipc::{encode_request,decode_response}` from Rust. C/C++ clients can
encode the following fixed widths; native struct padding is never transmitted.
All management integers are little-endian. Complete frames, including the prefix,
are limited to 8192 bytes. Unknown tags, trailing bytes, invalid booleans, excessive
counts and non-increasing request IDs fail closed.

A request contains `u32 body_length`, `u16 version`, `u64 id`, `u64 deadline`, then
`u8 command_tag` and its arguments. A device key is `(u64 id, u64 generation)`;
a session ID is `(u64 epoch, u64 serial)`; a peer fingerprint is 32 bytes.

| Tag | Command | Arguments |
| --- | --- | --- |
| 0 | List | none |
| 1 | Authorize | device key, peer, u8 allow (0/1) |
| 2 | Attach | u16 count, device keys (1–256, further host limits apply) |
| 3 | Detach | session ID |
| 4 | Status | session ID |
| 5 | Heartbeat | session ID |
| 6 | Subscribe | none |
| 7 | Poll | u64 subscription |
| 8 | Unsubscribe | u64 subscription |
| 9 | Forget | session ID |
| 10 | Shutdown | none |
| 11 | Permissions | device key |
| 12 | Info | none |
| 13 | Inspect | device key |
| 14 | Sessions | none |
| 15 | Unshare | device key |

A response contains `u32 body_length`, `u64 id`, `u16 error`. A nonzero error ends
the frame. A successful response then contains `u8 reply_tag` and its value.

| Tag | Reply | Value |
| --- | --- | --- |
| 0 | Done | none |
| 1 | Devices | u16 count; repeated device key, u8 busy |
| 2 | Session | session ID, peer, u8 state, u16 terminal error, u16 device count, keys |
| 3 | Subscription | u64 ID |
| 4 | Event | u8 present; if present: u64 sequence, u8 kind, u8 session-present, optional session ID |
| 5 | Peers | u16 count, fingerprints (maximum 255) |
| 6 | Info | u16 version, u64 epoch, u64 monotonic milliseconds |
| 7 | Inspection | device key and descriptor topology (below) |
| 8 | Sessions | u16 count, session IDs |

States: 0 Acquiring, 1 Active, 2 Draining, 3 Quarantined, 4 Closed. Event kinds:
0 Inventory, 1 Session, 2 Resync. Errors: 1 Invalid, 2 State, 3 Unsupported,
4 Unavailable, 5 Denied, 6 NotFound, 7 Version, 8 Deadline, 9 Cancelled,
10 Exhausted, 11 Internal, 12 Cleanup. Zero means no error.

Inspection encodes its device metadata using the canonical 320-byte USB/IP import
reply record (that record alone uses USB/IP network byte order), then a u8
configuration count. Each configuration contains u8 value and u16 interface count.
Each interface contains u8 number, alternate, class, subclass, protocol and endpoint
count. Each endpoint contains u8 address and kind, u16 maximum packet size, u32
service bytes, u8 interval and u32 supported flags. Endpoint kinds use USB values:
0 control, 1 isochronous, 2 bulk, 3 interrupt. A topology exceeding the bounded
management frame returns Exhausted. This metadata is not a USB transfer channel.

Exporter visibility and permissions remain enforced in the engine. The importer
host supports discovery, inspection after import, single-device attach/detach,
status, sessions, events, forget, info and shutdown. Exporter-only policy mutations
return Unsupported there. Importer liveness is maintained by its authenticated
transport; local UI clients need not send heartbeats.
