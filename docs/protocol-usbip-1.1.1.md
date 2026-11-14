# USB/IP 1.1.1 codec

`crates/knightbus-usbip` is an independently authored MIT Rust library with no
third-party dependencies or unsafe code. Its inputs are byte streams and original
request metadata. It owns no sockets, USB handles, policies, threads or sessions.

The wire contract comes from the [published Linux USB/IP specification](https://www.kernel.org/doc/html/latest/usb/usbip_protocol.html).
The [Linux v6.12 wire-structure declarations](https://github.com/torvalds/linux/blob/v6.12/drivers/usb/usbip/usbip_common.h)
clarify signed fields and the four ISO descriptor words omitted from that document's
tables. These are field-layout references, not sources of copied implementation.
No third-party forwarding implementation or fixture was imported.

## Wire map

Protocol integers use big-endian encoding. USB setup and payload bytes remain
opaque; embedded USB little-endian words are never swapped. No Rust struct layout,
packed cast, native endianness or host errno values define the wire representation.

| Message | Code | Fixed prefix | Following bytes |
| --- | --- | --- | --- |
| DEVLIST request | `0x8005` | 8 | None |
| DEVLIST reply | `0x0005` | 8 + 4-byte count on success | Device record then interfaces, repeated per device |
| IMPORT request | `0x8003` | 8 | 32-byte bus ID |
| IMPORT reply | `0x0003` | 8 | One device record on success; no interface records |
| SUBMIT | 1 | 48 | OUT data, then ISO records |
| UNLINK | 2 | 48 | None |
| RET_SUBMIT | 3 | 48 | IN data, then ISO records |
| RET_UNLINK | 4 | 48 | None |

Management prefixes contain version `0x0111`, 16-bit operation, and 32-bit status.
Request status must be zero. Nonzero reply statuses are preserved; error replies
contain only the eight-byte prefix. Successful device lists may be empty.

A device record is 312 bytes: path[256], bus ID[32], bus/device/speed u32,
vendor/product/BCD revision u16, then six u8 class/configuration/interface fields.
Each list interface is class/subclass/protocol u8 plus zero padding. The model's
`Device.interfaces` and `ListedDevice.interfaces.len()` must agree. IMPORT retains
the advertised interface count but does not append interface records.

Every transfer prefix has command/sequence/device/direction/endpoint u32 at offsets
0/4/8/12/16. The remaining layout is:

| Offset | SUBMIT | RET_SUBMIT | UNLINK / RET_UNLINK |
| --- | --- | --- | --- |
| 20 | flags u32 | status i32 | target sequence u32 / status i32 |
| 24 | buffer length i32 | actual length i32 | zero padding through byte 47 |
| 28 | start frame i32 | start frame i32 | — |
| 32 | ISO count i32 | ISO count i32 | — |
| 36 | interval i32 | error count i32 | — |
| 40 | USB setup[8] | zero padding[8] | — |

Positive buffer lengths and error counts have unsigned Rust representations but
are checked against their signed wire domain. Endpoint numbers are 0–15, without
a direction bit. Replies encode device, direction and endpoint as zero. UNLINK
encodes direction/endpoint as zero and has its own sequence distinct from its target.
Signed statuses are preserved, including cancellation `-104` and too-late `0`.

## Transfer and ISO semantics

Control, bulk and interrupt share SUBMIT/RET_SUBMIT; there is no transfer-type field.
Endpoint descriptors supply type in a future backend. The codec preserves setup,
flags, interval, scheduling fields and short/error completions without invoking an OS.
It accepts both `-1` (documented non-ISO sentinel) and `0` (common peer representation)
for packet counts, retaining the received value. Other negative counts are invalid.
Signed scheduling fields, including `-1`, are retained for peer compatibility.

An ISO descriptor is four network-order words: original buffer offset, requested
length, actual length, signed status. The original offset plus length must fit
within the requested buffer; actual length must fit its packet. A completion's
sum of packet actual lengths must equal the header actual length, including OUT
completions that carry no data. Error count cannot exceed packet count.

Framing follows the header's byte count: OUT SUBMIT carries `transfer_buffer_length`
bytes, and IN RET_SUBMIT carries `actual_length` bytes. The ISO descriptor array
follows that payload, never vice versa. IN completions contain concatenated packet
data; original buffer gaps are represented by descriptor offsets, not wire padding.
For OUT, producers must supply the wire buffer matching the declared length and
valid descriptor ranges; the codec never silently inserts, removes or interprets
payload padding. It does not scatter/gather into an OS URB.

ISO wire support is not proof of native ISO scheduling or device support.

## Public API and stream ownership

`encode(&Message, Limits)` validates the model and counts its output before allocating.
`decode(bytes, Context, Limits)` parses exactly one prefix and returns `(Message,
consumed_bytes)`. A caller retains subsequent coalesced bytes. `Error::Incomplete(n)`
requests a minimum total prefix size for the next parsing step; at EOF it means
truncation, except an empty stream between frames is clean EOF.

`Context` explicitly selects management, importer requests, or exporter replies.
`ReplyContext` supplies the original sequence, direction, requested capacity and
ISO count. A RET_SUBMIT without matching context returns `Error::Context(sequence)`.
A session can peek that sequence, resolve it in its outstanding-request map, then
retry the unchanged prefix. The zero direction in a reply header must never be
used to infer payload presence. Actual length is bounded by the original request.

`Decoder::push` buffers a single frame incrementally and reports how much input it
consumed. After completion, create the next decoder with the appropriate context;
this lets the caller switch from IMPORT to transfers or select another request.
After a fatal error it is poisoned. `finish` distinguishes partial-frame EOF.
It does not scan for a new header after malformed input.
Device-list framing retains a cursor over completed records, so incremental
parsing does not repeatedly scan earlier devices.

`read_message`/`write_message` adapt generic blocking `Read`/`Write`, retry interrupted
operations and handle short reads/writes. Reads stop exactly at the frame boundary.
Blocking reads fill the frame buffer directly up to validated boundaries, without
an intermediate scratch buffer or a fixed read-size cap. Transfer headers are
stored separately so the completed body allocation can become the message's
`Vec<u8>` payload without a second payload allocation or copy. ISO records are
validated and decoded before their wire bytes are truncated from that buffer.
Empty payloads retain no body allocation. Small/sparse payloads are copied instead
when the body buffer's spare capacity exceeds one eighth of the payload length;
this avoids retaining excessive space in queues of completed messages. This is
an internal memory policy, not a protocol restriction. Borrowed `decode(&[u8])`
continues to copy its payload so the caller retains ownership of its input.
They neither create sockets nor flush/close the caller's stream. Nonblocking users
use `Decoder` or retain an encoded buffer and write offset; `WouldBlock` from the
blocking adapters is an error, not permission to restart a partially sent frame.
Transport deadlines, backpressure and connection teardown belong to the caller.

## Resource and validation boundary

Defaults are configurable local budgets: 2 MiB encoded frame, 1 MiB transfer buffer,
256 devices and 1,024 ISO packets. They are not USB/IP constants or claims of native
backend limits. Length arithmetic is checked before indexing/reserving memory.
Stateless decoding allocates bodies only after the bounded complete frame exists.
Incremental decoding reserves only bounded framing stages and does not copy trailing
coalesced data. Streaming transfer payloads normally reuse body storage; copied
fallbacks, management records and decoded ISO records still require additional
storage. Retained payload capacity may include a bounded amount of unused space.
Callers must budget actual allocation capacities and outstanding frames/sessions,
not just logical payload lengths. No unlimited queue is hidden here.

`Decoder::buffered_len()` counts received frame bytes, including the saved header;
it is not a memory quota metric. `Decoder::allocated_capacity()` reports the heap
buffer's capacity, including unused reserved space. For example, a 48-byte transfer
header plus one body byte can reserve a declared 1 MiB body while `buffered_len()`
reports 49. Capacity excludes the inline decoder/header, allocator overhead,
temporary parsing allocations and returned messages. After completion the buffer
is released or transferred, so the caller must account for message allocations
separately.

`encode`, `decode`, `Decoder::push`, and the blocking stream helpers require
explicit allocation admission. Their caller-supplied
hook approves additional bytes before each requested allocation, including decoded
ISO/management records and temporary growth storage. Allocation refusal is fatal to
an incremental decoder. Keep a consistent admission owner throughout decoding and
retain its charges through returned messages; capacity reporting alone cannot do
this. Hooks conservatively count temporary/reallocated storage; allocator-reported
excess is charged before exposure and rejected excess storage is discarded.
`knightbus-core` supplies the owned `Budget::decoder` / `DecodedMessage` bridge and
charged encoding buffers without introducing a codec dependency on the engine.

`SubmitHeader` holds allocation-free request metadata. `Submit::into_parts` and
`SubmitHeader::with_payload` move payload/ISO ownership without copying, while the
existing `Submit` representation and wire format remain compatible.

Fixed strings require a NUL and zero-filled tail, accept non-UTF-8 bytes and never
read beyond their field. Reserved bytes, version, opcode, phase, direction, endpoint,
request status, lengths, count consistency and ISO ranges are checked. Unknown speed
codes, URB flags and signed completion statuses are preserved rather than mis-mapped
into a host API. Backends must validate supported flags and USB request semantics.

## Verification

The [24 golden vectors](../crates/knightbus-usbip/tests/fixtures.txt) are original
synthetic bytes independently generated from published layouts using Python
`struct.pack`, not this Rust encoder or upstream fixtures. They are consolidated
in one named-section file beside the tests. Test VID/PID `1234:abcd` is invented.
The one-off generator is retained locally rather than shipped as another tool.

Return vectors use original-request sequence `10203040`, capacity 64 for non-ISO,
8 for ISO IN and 5 for ISO OUT, with two ISO packets where applicable. Reply wire
direction is zero even for IN payloads. ISO IN completion data is packed: original
offsets 0/5, requested lengths 3/3, actual lengths 2/1, payload `abc`.

Tests check exact bytes, field meanings, all split points, EOF, coalescing,
short/interrupted I/O, invalid input and generated transfer round trips. Both
[libFuzzer targets](../crates/knightbus-usbip/tests/fuzz/README.md) also have
assertions exercised by ordinary offline tests.

External checks passed against stock Linux management tools, Wireshark and the
Linux 6.12.67-linuxkit kernel importer. The kernel lab covered native enumeration,
control/bulk/interrupt/ISO IN and OUT, short/error completions, packed ISO payloads,
both unlink orderings, non-ISO counts `0` and `-1`, and clean detach. USBFS
cancellation status (`-ENOENT`) differs from wire RET_UNLINK (`-ECONNRESET`);
the test checked each boundary separately. Wireshark 3.6.2 checked selected headers
only and has known bodyless-error and `-1` count limitations.
