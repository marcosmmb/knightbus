# Shared control contract

Embedded and Service modes must expose the same logical engine operations through
`ControlConnection` and `IpcControl`. M6 implements these Rust adapters and a
bounded framed Service harness over the same engine. See [M6](milestone6-validation.md)
for concrete behavior and limits. Production OS-authenticated IPC, the public SDK
schema and C headers remain M12 work.

## Operations and authority

| Operation | Intended semantics / authority |
| --- | --- |
| List devices | Exporter filters inventory for an authenticated importer; caller may inspect only peers it can manage |
| Authorize device | Exporter administrator creates/revokes a device grant; paired importers cannot self-grant |
| Attach | Local operator permission plus remote exporter grant; validate the current physical generation |
| Detach | Lease owner or exporter administrator initiates cancellation/drain; distinguish completion from requested cancellation |
| Get session/status | Authorized lifecycle, capabilities and sanitized failure; no disclosure of hidden leases/devices |
| Subscribe/unsubscribe | Bounded, filtered events; acknowledge callback retirement before caller context can be freed |

Trusted adapters supply caller identity outside request arguments. Embedded uses
the host's OS identity and configured management policy; IPC verifies the OS
principal and authorizes each operation. A requested peer ID is not a credential.

Long-running operations must not block a GUI thread. Use operation identifiers,
monotonic deadlines and terminal results that distinguish success, cancellation
and failed cleanup. Serialize mutations within a session. Reconnect requires
snapshot reconciliation rather than blindly replaying attach/grant operations.

## Errors and events

Both adapters must preserve the same stable error meanings: invalid argument or
state, unsupported operation, unavailable backend, permission denied, not found,
version mismatch, deadline exceeded, cancellation, resource exhaustion and internal
failure. M6 assigns v1 codes 1–12 respectively to invalid, state, unsupported, unavailable,
denied, not-found, version, deadline, cancelled, exhausted, internal and cleanup.
The Rust `Error` enum is the source of truth. Native diagnostics are sanitized metadata, not substitutes for these
errors. Unauthorized requests must not reveal hidden inventory.

Events use subscription-local sequence numbers, session/generation and typed lifecycle,
visibility, grant or resynchronization information. Bound events by count and
aggregate bytes. Overflow/gaps require an authorized snapshot. M6 applies quotas per peer across
connections and reserves administrator slots plus their queue memory. Inventory
notifications go only to administrators and affected grantees, including revocations.
Closed session details are best-effort history and may be reclaimed under pressure;
stale IDs never select a new session. Dispatch outside
engine locks; slow clients must not block USB completion or cancellation.

## Local IPC

Use OS-authenticated Unix domain sockets or local Windows named pipes, with
operation-specific permissions; see [security model](security-model.md).
Version the local protocol independently of USB/IP and the C ABI. Reject a major
mismatch and unknown required features; negotiate compatible minor versions.

Prefer length-prefixed UTF-8 JSON for local control, with strict frame/string/
nesting bounds, duplicate-key rejection and fragmented/coalesced read handling.
USB reports and URBs never travel through this management channel. Requests and
responses correlate by ID; events are distinct. Represent wide integer IDs without
precision loss in JavaScript clients. Review exact framing, limits and schema
alongside the parser and authentication tests in M6/M12.

## Embedded C ABI

Use fixed-width scalars, opaque handles and versioned, size-tagged C structures.
Do not expose Rust or C++ container layouts. Define explicit output-buffer release,
callback context lifetime and stale-handle behavior. Handle values are not security
credentials. Ordinary Rust panics must not unwind into foreign code; allocation
failure and process termination are outside that guarantee.

Callbacks run outside engine locks and may not unwind. Unsubscribe/shutdown must
acknowledge retirement before host memory is released. Offer polling/event queues
where callbacks are unsuitable, with identical semantics. Concrete ABI symbols,
allocation limits and ownership functions belong to M12, with native C/C++ tests.
