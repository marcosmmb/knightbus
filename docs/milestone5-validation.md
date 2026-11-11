# Milestone 5 validation

2026-11-10, branch `milestone5`, uncommitted working tree. **The USB/IP 1.1.1 wire
codec and its M5 checks pass**, including stock Linux kernel interoperability for
the synthetic-device scope below. This is not a USB forwarding engine or backend.

## Retained implementation

One MIT, std-only crate covers all eight messages, device/interface records,
control/bulk/interrupt/ISO fields, signed statuses, direction-specific payloads,
original-request reply context, bounded allocation and fragmented/coalesced streams.
No sockets, USB APIs, session engine, driver, daemon, C ABI or GUI are implemented.

Following the owner's review preference, reusable verification lives beside the
crate: `tests/protocol.rs`, one `tests/fixtures.txt` containing 24 original vectors,
and an isolated `tests/fuzz/` workspace. The lab scripts, generators and pipe
examples were removed from commit scope and preserved in a local snapshot. Their
completed experiments remain recorded here; they are not maintained CI checks.
The vectors were verified byte-for-byte against the original files after moving.

The layout cleanup preserved the original four product `src/*.rs` files (SHA-256
`791768d7ead76303757bc0cba1f83d07b0ff05c23735a0743d9dec7039a7c502`).
The first 2026-11-11 review fixes added a device-list framing cursor and direct
reads into the frame buffer (source SHA-256
`0940e50452d180be2dc614bed99578eb79ff828ef9873481fd28af9e849f22a7`).
The subsequent payload change moves suitable body allocations into the message
and copies small/sparse payloads to avoid excessive retained capacity. The public
API remains unchanged. The current source SHA-256 is
`6695f3f62b001e9a2fd47b2d74b5a8955de5dd72424464b851c04c13e064440f`:
sort those four repository-relative paths; hash each UTF-8 path, NUL, contents, NUL.
Raw logs, captures, credentials, fuzz corpus and build output remain outside Git.

## Checks performed

The external/platform measurements below predate the review fixes; the final rows
record their local verification. Kernel interoperability and remote platform runs
were not repeated for these changes.

| Check | Observed result |
| --- | --- |
| macOS 27.0 (26A428), Apple Silicon | Rust 1.85.0 debug/release: 16 integration tests and one documentation example pass; formatting, Clippy and rustdoc with warnings denied pass |
| Additional macOS compiler | Rust 1.96.0 native tests/coverage pass |
| WEASLEY, Windows 11 x64 (10.0.22631) | Rust 1.85.0 and 1.97.1 debug tests pass; 1.85.0 release and documentation tests pass |
| Snape CT, Ubuntu 22.04.2 x64, kernel 6.8.12-43-pve | Rust 1.85.0 debug/release, documentation and Clippy pass |
| Clean copy before layout cleanup | Empty Cargo cache/separate target directory: offline tests and fixture/dependency checks pass |
| Golden vectors and parser tests | All 24 exact packets, field meanings, truncated prefixes, split points, limits, malformed input, ISO layout, coalescing, short/interrupted I/O, EOF and poisoned state pass |
| Coverage-guided fuzzing | ASan, nightly-2026-05-25 on macOS: decode 18,678,136 executions; stream 2,072,977 executions; 46 seconds each, no failure |
| Product-source coverage | cargo-llvm-cov 0.8.7 / Rust 1.96.0: 96.89% lines, 92.49% regions; excludes examples/fuzz support, not a correctness proof |
| Stock Linux management tool | Eight loopback exchanges pass: empty/two-device/error DEVLIST and failed IMPORT, each contiguous and fragmented |
| External dissector | Wireshark 3.6.2 checks selected headers for 23 distinct fixtures across 65 synthetic frames; limitations below |
| Stock kernel importer | Successful import/enumeration, all transfer types in both directions, short/error completions, unlink orderings and clean detach pass |
| Hosted CI | Portable matrix and Linux fuzz job configured; not pushed or executed on GitHub |
| After layout cleanup, macOS | Formatting, Clippy, debug/release tests, documentation tests/build and explicit corpus seeding pass; relocated ASan targets pass six-second smoke runs (decode 2,499,469 executions; stream 453,956); fuzz lock unchanged |
| 2026-11-11 review fixes, macOS / Rust 1.85.0 | Formatting, Clippy, debug/release (17 integration tests), documentation test/build pass. Retained regression checks verify full-size body reads, short reads, truncation, limits and no overread. ASan fuzz smoke: decode 3,577,450 executions; stream 698,619; 11 seconds each, no failure |
| 2026-11-11 payload ownership, macOS / Rust 1.85.0 | Formatting, Clippy, 18 integration tests in debug/release, documentation test/build pass. Added checks for allocation reuse, compact small/sparse/empty payloads, the spare-capacity threshold, partial-header EOF and coalesced input. Seeded ASan fuzz: decode 4,348,259 executions; stream 1,040,514; 11 seconds each, no failure |

### Payload ownership benchmark

An external temporary harness compared the previous codec with this revision on
Apple M4, Rust 1.85.0 release builds. Nine paired batches per configuration used
alternating AB/BA order, rotating 64 MiB inputs, and 0/32 retained messages.
Separate runs measured allocations at queue depths 0/32/128, individual service
times and four parallel workers. Source copies, harness and raw results stay local.

With 32 retained messages, whole-input incremental decoding took 2.463 → 1.341 µs
for 64 KiB bulk and 1.255 → 0.740 µs for 32 KiB ISO (median batch averages).
The 1 MiB no-queue case reduced peak live requested heap from 2,097,200 to
1,048,576 bytes. Small and sparse ISO queue retention matches the baseline after
the copy fallback; dense 32 KiB ISO retains about 1.5% extra capacity to avoid copying.
A focused borrowed-decode repeat found no consistent slowdown. Fragmented 1 MiB
transfers with 32 retained messages had overlapping ranges (37.586 → 36.495 µs
medians); no reliable improvement is claimed for that case. These are in-memory
codec measurements on one workstation, not webcam throughput or latency guarantees.

The CT used Ubuntu `linux-tools-5.15.0-203{,-generic}` version `5.15.0-203.213`,
reporting `usbip-utils 2.0`, executable SHA-256
`4161dd92bff26de104e74e3a10e3c5994701b28007f112cd787047a8f520d3f6`.
The dissector package was `tshark 3.6.2-2`. These were external tools, not dependencies.

## Stock kernel experiment

Docker Desktop 4.59.1 (217750), Engine 29.2.0, Linux 6.12.67-linuxkit aarch64,
stock `usbip-utils 2.0` and built-in `vhci_hcd` imported an original synthetic
vendor-class USB 2.0 high-speed device. The VM tool's SHA-256 was
`240070d29d4dffbcf20efeab2561f9ea4723349277dd3afaac75436e89d02afb`.
The disposable image was `kindest/node:v1.31.1` at digest
`sha256:cd224d8da58d50907d1dd41d476587643dad2ffd9f6a4d96caf530fb3b9a5956`.
Its entrypoint was overridden with `sleep`; no Kubernetes service ran.

An original Rust fixture used the product codec over standard input/output. A
loopback relay copied opaque bytes; a separate USBFS consumer submitted native
URBs. Linux generated/consumed the wire messages. No physical USB device was used.
Two full runs passed, with 30 SUBMIT requests each including enumeration:

- DEVLIST, IMPORT, descriptor/configuration enumeration, and selection of alternate
  setting 1 from the zero-bandwidth default setting.
- Control IN/OUT/short/zero; bulk/interrupt IN and OUT, short IN, stall, and errors
  retaining positive actual length.
- ISO IN/OUT: lengths `[8, 12]` completed fully; lengths `[8, 5, 3]` returned actuals
  `[3, 5, 0]`, statuses `[0, 0, -18]`, total 8 and error count 1. IN data appeared at
  original offsets, including the gap after the short first packet.
- Cancellation won with RET_UNLINK `-104` and no RET_SUBMIT; completion won with
  RET_SUBMIT followed by RET_UNLINK `0`. Each produced one native completion and
  subsequent transfers worked. USBFS DISCARDURB reported `-ENOENT` (`-2`) in both
  cases; the completion-won case retained all 68 bytes. API and wire statuses differ.
- One run used non-ISO return count `0` with fragmented relay writes; the other used
  `-1` with ordinary writes. Linux accepted both. Exact split coverage is in unit tests.
- Interface release, owned port free, sysfs child removal and successful fixture/
  relay exit were checked. All 16 VHCI ports were free before and after; containers
  were removed. The privileged lab exposed no network port outside its container.

Final count-zero/sentinel USBFS result SHA-256 values respectively:
`c31deeab62159d0d0b82496c95dbe5134ff8554053abd56ff976d7b9dc724079`,
`ebbefa69274df273edaf4ec6339a683a84cba8bcafaee4634aeee6cfb772c217`.

## Reproduction and limits

Use [README](../README.md) for the retained portable checks and
[the fuzz instructions/dependency inventory](../crates/knightbus-usbip/tests/fuzz/README.md)
for fuzzing. Tests require no Python, USB hardware or elevation. An explicit ignored
Rust test writes local fuzz seeds; ordinary test runs do not write a corpus.
The external lab harness is intentionally outside this repository, so this checkout
alone does not reproduce those historical interoperability experiments.

The kernel proof covers the tested synthetic device and stock importer. It does
not prove a stock exporter, physical USB behavior, ISO scheduling/timing, other
kernels, endurance, production backends or distribution. M1–M3 gates remain unchanged.
Wireshark 3.6.2 omits bodyless DEVLIST errors/some non-ISO fields and misframes the
`-1` count sentinel; its selected-header check is not full conformance validation.
The live stock peer checks errors separately, and the native kernel accepts `-1`.
Fuzzing is finite; the successful runs used a 512 MiB process cap and 32 MiB ASan
quarantine after an initial 256 MiB cap was exceeded. Codec budgets did not change.
