# Independent KnightBus requirements and design

2026-11-08. Proposed design, no approved ADR and no implementation. Evidence classifications, pin limitations and source URLs are in [the reference ledger](reference-projects.md). All acceptance tests below are **planned, not run**. Inspiration is functional; package layout, algorithms, IPC and tests must be authored from requirements and official APIs.

## Idea-to-requirement matrix

| ID / problem | Inspiration and evidence | Independent Rust design requirement | Acceptance / milestone |
| --- | --- | --- | --- |
| R01: stream boundaries and parsing | Linux L1 SPEC; Extender manifest STRUCTURE | Pure byte codec, typed management/data states, checked arithmetic; no OS handles or async runtime in codec | Every fragmentation point, coalescing, malformed lengths and independent vectors; M3 subset, M5 complete |
| R02: completion races | Linux L1 SPEC; usbipd-win AttachedClient STRUCTURE | One session owner arbitrates response commitment and cancellation; backend lifetime distinct from wire completion | Cancel-before-submit, simultaneous completion, already-committed completion, late callbacks; M3/M6 |
| R03: reusable backend APIs | Extender manifest and sing-usbip client STRUCTURE | Separate physical-device operations and virtual-attachment operations; portable capability values with explicit unsupported results | Fake backend failure injection; second OS without wire-code changes; M4/M15 |
| R04: exporter authorization | usbipd-win bind CLAIM, NKS public ACL CLAIM; user invariant | Authenticated importer key identity + exporter-managed device grant; filter inventory/events; recheck grant and physical generation on import | Unknown importer sees no devices; guessing busid, cloned VID/PID and revoked grant denied; M3 basic/M6/M11 |
| R05: event notifications | sing-usbip control STRUCTURE/README CLAIM | Optional versioned control connection with filtered snapshot, monotonic revision, bounded subscriber queue and resync | Lost/duplicate/out-of-order event triggers snapshot; raw USB/IP peer works without control; M6/M11 |
| R06: encrypted peer lifecycle | Extender TLS dependencies STRUCTURE; AirUSB pairing CLAIM | Rust user-mode mTLS tunnel with pinned peer identities and explicit enrollment; each data stream bound to current grant/lease | Unknown/revoked/wrong peer and replay denied; no plaintext downgrade; M3 lab/M11 production |
| R07: macOS ownership | usbipd-mac CLAIM; Apple A1/A2 SPEC; AirUSB CLAIM | Original Rust inspector separates enumerate/open/capture/transfer/release; per-interface capability and process context | Actual joystick interrupt and control traffic plus local recovery, SIP on; M2 |
| R08: native Windows attach | usbip-win2 STRUCTURE; Microsoft W1–W6 SPEC | Original Rust UdeCx/KMDF driver; validated bounded IOCTL boundary; TLS/USBIP parsing stays in user mode | Native class binding, control and endpoint transaction through userspace, cancellation and uninstall; M1 |
| R09: privilege separation | LAN Bridge README CLAIM; usbipd-win service CLAIM | Narrow privileged device broker, authenticated local IPC and service-only driver ACL; no arbitrary shell execution | Unprivileged caller cannot claim/install; stale service handle rejected; M1/M2/M6 |
| R10: exclusive leases | LAN Bridge README CLAIM | Exporter monotonic expiry, owner identity, random lease ID and generation; reconnect obtains fresh grant | Stop heartbeat, kill either process, delayed renewal, restart reconciliation; M6/M11 |
| R11: multi-device attach rollback | LAN Bridge README CLAIM | Reserve entire set before claims; compensation journal per device; explicit partial-cleanup outcome | Failure at each acquisition/attach step leaves earlier unrelated sessions intact; M6/M11 |
| R12: safe detach | usbredir header STRUCTURE; Microsoft W6 SPEC | Freeze ingress, invalidate generation, cancel, drain backend callbacks, remove virtual device, release physical ownership | No callback after free; deadline bounded; failed cleanup quarantines resource; M1/M2/M3 |
| R13: UI discovery | OpenUSBIP README CLAIM and dependency STRUCTURE | Optional mDNS advertises endpoint/capabilities only; authenticate exporter before retrieving filtered inventory | Spoofed mDNS cannot pair or authorize; stale address harmless; M12 |
| R14: diagnostics | wolfusb README CLAIM | Original CLI reports role, operation phase, OS status, unsupported capability and sanitized identifiers | Claim-denied differs from network failure; payload/serial/key redaction checks; M2/M12 |
| R15: Linux oracle | Linux L1 SPEC; reference backend CLAIMS | External stock Linux USB/IP used for isolated interoperability; Rust exporter owns protocol and policy | List/import/control/bulk/interrupt/unlink with pinned tools/kernel; M3/M5/M10 |
| R16: future Windows exporter | usbipd-win lifecycle CLAIM/STRUCTURE | Explicit opt-in, preserve original binding identity; restore only changes owned by this operation | Capture failure/uninstall leaves original driver usable; M15 |
| R17: callback/transport boundary | usbredir header STRUCTURE, different protocol | Completion messages own buffers; backend cannot write wire packets; transport has no platform pointers | Transport shutdown while completion arrives; independent mock transport/backend; M4/M6 |
| R18: transfer coverage | Linux SPEC; AirUSB unsupported-endpoint CLAIM | Capability per transfer type/direction/device/OS; reject unsupported endpoints before import | Control/bulk/interrupt both directions and per-packet iso results; M3 minimum/M10 full |
| R19: licensing/source exclusion | All references; proprietary NKS boundary | No forwarding crate, driver binary, source fixture or translated API copied into product | Dependency and repository inventory review; all milestones |
| R20: bounded resources | Observed unbounded usbipd-win reply channel; user invariant | Reserve byte and request credits before transfer allocation/submission; reserve cleanup capacity independently | Slow importer, stalled backend and descriptor bombs never exceed configured caps; M3/M6 |

## Proposed package and privilege boundaries

Create no crates in M0. At M4, use the smallest packages justified by the gate results:

| Responsibility | Proposed package / interface property |
| --- | --- |
| Wire codec | `knightbus-usbip`: checked decode/encode, spec-defined values, no I/O, policy or platform code |
| Portable ownership model | `knightbus-core`: transfer IDs, errors, capabilities, generation IDs and state transitions; no Moonlight/Sunshine knowledge |
| Export decisions and physical sessions | `knightbus-exporter`: policy snapshot plus serialized lease owner, backend claim/submit/cancel/release |
| Remote attachment orchestration | `knightbus-importer`: virtual backend attach/requests/completions/unplug; never grants itself physical access |
| Security and control | `knightbus-transport` and `knightbus-control` only when complexity warrants separation: authenticated byte streams, enrollment, events, leases |
| OS backends | Rust platform packages isolate unsafe FFI, callbacks and privilege boundaries; Windows kernel package isolated from user-mode workspace settings |
| Applications | CLI/services call library interfaces; integration adapters remain outside libraries. IPC versus C ABI deferred to M4 ADR |

An exporter backend receives validated USB operations, not network buffers. A claim returns an exclusive handle with a physical-generation token and a capability set. Submit returns a backend token; cancellation acknowledges request receipt separately from eventual callback retirement. Release is explicit asynchronous cleanup with a deadline; dropping a future does not imply OS cancellation completed. An importer backend emits local requests and consumes validated completions; it cannot access TLS keys. Each unsafe wrapper must state pointer provenance, ownership, callback lifetime, thread/IRQL rules, pinning and cancellation obligations. No unwinding through OS callbacks.

## Independent state machines

**Session:** disconnected → authenticating → authorized → reserving → claiming → importing → active → quiescing → draining → releasing → closed. Every failure enters cleanup at the last owned-resource boundary. Management DEVLIST is a distinct short-lived exchange; a successful standard IMPORT changes that connection to data mode. Authorization context is provided by the outer authenticated tunnel, never added to USB/IP headers. An import success is committed only when a valid claim and lease exist. A failed response write releases them. After network loss, old traffic cannot resume a generation.

**Transfer:** validated → credits-reserved → backend-pending → response-ready → response-committed → retired. Cancel can race any non-retired state. One owner makes the wire decision; backend callbacks can outlive that decision, so their allocation remains alive until retirement. A successful unlink suppresses a later RET_SUBMIT for its target, as required by L1. If RET_SUBMIT was committed first, the later unlink receives the specified too-late result. Keep unlink's own sequence distinct from its target. Never reuse a live sequence; close/re-establish before wrap could collide. Duplicate requests, wrong connection generation or invalid response correlation terminate the session safely.

**Device:** present/unshared → approved → reserved → claimed → exported → draining → returned. Removal moves any state to absent and invalidates every old lease/handle. Failed restoration moves to quarantined; inventory must not advertise availability merely because the socket closed. Reinserted identical VID/PID is a new generation requiring policy evaluation.

**Policy transaction:** take authenticated identity and current inventory generation → filter explicit grant → atomically reserve device → claim → revalidate grant/generation → commit. Revoke during claim aborts; revoke during active use initiates detach. VID/PID, serial strings, mDNS and importer-provided client IDs are selectors or hints, never credentials. Return generic denials where detail would reveal hidden devices.

## Leases and rollback

Proposed experimental defaults: heartbeat every 5 s, exporter lease expiry 20 s from last authenticated renewal, cleanup deadline 5 s. These are bounded lab choices to measure, not protocol constants or approved production SLOs. Only the exporter clock determines expiry. Renewals carry owner/session/generation and cannot resurrect an expired lease. Pairing keys persist; ephemeral session capabilities do not survive restart. Persist a minimal operation journal before claiming and reconcile OS ownership on startup before making a device available.

Batch acquire: authorize all requested generations, reserve in deterministic device order, claim each, import/attach each, then commit the set. On failure, compensate completed steps in reverse order. Record each cleanup result; no distributed atomicity claim. Do not unbind devices owned by another session. Cleanup failure produces a visible partial result and quarantine. Revocation or expiry overrides a partially committing batch. Test same-batch conflicts, opposite-order concurrent batches, crash after every journal write and detach that unexpectedly removes other virtual ports; do not silently reattach revoked devices.

## Security and backpressure design

Prefer rustls using audited standard TLS implementation, mutual peer authentication and exporter-approved enrollment. Enrollment tokens are short lived, single use and never loggable. TLS 0-RTT must not authorize imports. Bind separately authenticated control/data connections to the same peer and fresh one-use session capability before exposing unchanged USB/IP bytes. No opportunistic plaintext fallback. Production raw port 3240 stays unavailable; compatibility testing uses an explicit isolated listener or authenticated tunnel gateway applying identical exporter policy.

Proposed M3 limits: 4 devices, 64 outstanding transfers/device, 256 KiB/transfer, 8 MiB aggregate buffered transfer data, 64 KiB descriptor/control budget, 256 iso packets/request. Check all sums/products and platform integer conversions before allocation. Limits can reject otherwise legal devices and must be reported; no silent truncation. Prioritize cancellation/control credits so a full data queue cannot prevent detach. Bound subscriber count and event queues; overflow requires resnapshot. Backpressure propagates to USB submissions; it must not turn into an unbounded completion queue.

Diagnostics record request ID, sizes, timings, phase and sanitized status only. No raw USB payloads, descriptors containing personal serials, credentials or pairing codes. Optional lab captures require explicit handling and sanitized publication. Plaintext USB may contain keyboard input or storage data even when payload type is unknown.

## Pending decisions

No exception is authorized here. M1 must resolve Rust UdeCx bindings, certification path and any original minimal non-Rust shim proposal. M2 must resolve capture/entitlement/process model against actual hardware. M3 scope reduction from the joystick requires owner approval. M4 selects license, final crate/API boundaries and integration IPC/C ABI; M11 finalizes enrollment, key storage and lease defaults. No production implementation begins under this research-only task.
