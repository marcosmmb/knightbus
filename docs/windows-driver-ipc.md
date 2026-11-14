# Windows private driver IPC, version 1

Development contract for the original M7 driver. All integers are little-endian;
all buffers are copied `METHOD_BUFFERED` IOCTLs with read/write access required.
No pointers, kernel handles, arbitrary memory reads or USB/IP frames cross this
boundary. Interface GUID: `53baf142-c628-47de-b891-5c71e38b9620`.
The interface ACL grants SYSTEM/administrators; hosting code must enforce peer
identity and device grants before opening an attachment.

| IOCTL | Input | Output |
| --- | --- | --- |
| ATTACH `0x22e424` | Exact descriptor envelope below | Empty |
| FETCH `0x22e404` | Four-byte endpoint-slot admission mask (31 bits) | 32-byte request header plus OUT payload; zero bytes when idle |
| REPLY `0x22e408` | 32-byte reply header plus IN payload | Empty |
| DETACH `0x22e40c` | Empty | Empty; owned native retirement acknowledgement |
| STATS `0x22e410` | Empty | 312-byte token/liveness snapshot |
| LIFECYCLE `0x22e414` | Empty | 128-byte lab-only object counters |
| RETIREMENT_READY `0x22e42c` | Empty | Empty; succeeds only with the protected-namespace final-destruction event bound |

ATTACH envelope: `KBWD` at offset 0, u32 version 1 at 4, actual USB speed
(1 low / 2 full / 3 high) at 8, u32 device length 18 at 12, u32 configuration
length at 16, raw device descriptor at 20 and raw configuration at 38. The
configuration is 9..4096 bytes and must match its advertised total length. Both
sides run the same allocation-free validation independently. Class-specific
bytes remain opaque; interface/alternate/endpoint topology is checked.

FETCH request header:

| Offset | Width | Meaning |
| --- | --- | --- |
| 0 | 4 | Version 1 |
| 4 | 4 | Endpoint address; zero for control, direction bit for other endpoints |
| 8 | 8 | Nonzero monotonic token, scoped to this controller's owner |
| 16 | 4 | Requested transfer length, at most 65536 |
| 20 | 4 | Payload length: requested length for OUT, zero for IN |
| 24 | 8 | USB setup packet for endpoint zero; zero otherwise |
| 32 | variable | OUT bytes |

FETCH initializes only its returned header and OUT payload. An empty poll returns
zero bytes, and the unused portion of the caller's maximum-sized buffer is never
exposed. STATS initializes exactly its returned snapshot fields.

REPLY uses the same version, endpoint and token fields. Offset 16 is actual
length; offset 20 is IN payload length (zero for OUT); offset 24 is u32 status
(0 success, 1 stall, 2 failure); offset 28 is reserved zero. IN bytes follow at
32. The driver validates exact length, original requested length, endpoint,
owner and live token before copying. A stale or canceled token cannot complete
a replacement request. Error translation is currently coarse; the native backend
does not claim full Linux errno/USBD status equivalence.

STATS consists of 39 u64 values: version, submitted, completed, canceled,
rejected, wrong-completion-IRQL count, next token, active-slot bitmask, then 31
slot tokens. Endpoint zero is slot zero; other slots follow unique endpoint
addresses in descriptor order. The worker compares token plus active bit and
emits cancellation once, only after delivering the corresponding transfer.
It does not fetch a replacement on that pipe until the remote owner retires.
FETCH rotates among eligible slots after each successful delivery. The worker
fetches a bounded batch of at most 31 eligible pipes before waiting, and its
mailbox also rotates delivery to the transport. A replenished low-numbered pipe
cannot monopolize either selection point.
Active batches continue without a timed sleep. Mailbox completions and shutdown
wake an idle worker; the 1 ms timeout only polls for native arrivals that currently
have no asynchronous notification.

LIFECYCLE is version plus 14 image-lifetime observations (child, endpoint and
owned queue create/cleanup/destroy; default-endpoint add; plugout calls/success;
reserved; unpublished-child delete) and a current owned-queue mask. These are
lab observations, not total WDF allocation counters. While attached, this query
also requires the owner file; after detach it is available to authorized handles.

ATTACH also requires the controller-owned virtual-root-hub filter to be ready;
a missing filter returns DEVICE_NOT_READY before publishing a USB child.
Actual configuration/alternate selections and endpoint-halt clearing use the same
FETCH/REPLY control path, then continue through UDE only after remote success.
The filter accepts raw control requests and the typed CLEAR_FEATURE_TO_ENDPOINT
URB; it checks the zero-length ENDPOINT_HALT selector and an owned nondefault
endpoint. A rejected remote clear completes the original control URB with failure;
higher-level Windows APIs can apply their own reset/recovery policy.

Each controller admits one owning file/child. All mutating IOCTLs after ATTACH
must use that file. The worker uses synchronous calls with a maximum scratch
buffer of 65568 bytes. Engine methods only exchange bounded mailbox entries;
no user buffer outlives a completed synchronous IOCTL. The kernel copies payloads
into fixed per-pipe storage, retains Windows request ownership, and arbitrates
reply/cancel under its spin lock. A separate DPC performs terminal URB completion.

DETACH and owner-file cleanup converge on one path. A failure does not authorize
reuse. The caller must retain/quarantine native resource credits until retirement
is proven; deadline expiry alone is never such proof. See the measured total
framework lifetime measurements in the [M7 findings](research/windows-rust-driver-feasibility.md).

The normal userspace adapter additionally owns the software controller. Its
`detach` result covers channel closure, observed controller/hub removal and the
final WDF destruction event, not just the private DETACH IOCTL. PnP absence alone
does not prove retirement while a native application holds a USB file open.
Each fixed slot uses a SYSTEM/Administrators-only `\Device\KnightBus.Controller.N`
manual-reset event, created exclusively with `NtCreateEvent`. The OS-owned parent
directory denies unprivileged creation even before an event exists; a per-object
ACL alone would not prevent squatting on the old Win32 `Global\` names.
The kernel opens the corresponding existing event by validated
controller identity before exposing interfaces, and signals it only on final WDF
destruction. Its kernel handle preserves the reservation even after importer death.
`RETIREMENT_READY` has exact zero input/output lengths and is checked before ATTACH
on the owned path; it supplies no handle and does not itself acknowledge removal.
Its code changed from `0x22e428` to `0x22e42c` with the namespace change so mixed
old/new drivers and adapters fail safely before attaching a USB child.
A failed removal/acknowledgement retains both its slot reservation and native
credits. The explicit `Controller::open` diagnostic path only retires
the child and is not the production attachment-lifetime path.
