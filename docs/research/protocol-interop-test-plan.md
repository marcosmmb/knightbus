# USB/IP interoperability and independent test plan

2026-11-08. **Planned only: no protocol suite or peer test exists or was run.** Primary authority: [Linux USB/IP 1.1.1 specification](https://www.kernel.org/doc/html/latest/usb/usbip_protocol.html). See [reference ledger](reference-projects.md) for upstream revision limitations. Generate vectors independently from the specification; do not copy its example payloads or upstream test fixtures.

## Protocol contract to implement after the gates

Use version 0x0111 and the documented network byte order for protocol integers. Embedded USB setup bytes and descriptor bytes retain USB-defined encoding; do not byte-swap opaque payloads as USB/IP words. DEVLIST is a management exchange followed by close. Successful IMPORT retains that connection for data, with exactly one imported device; multiple devices need separate data connections. Failed import has no successful-device body. The common data header is 20 bytes and the padded command header 48 bytes. Bound fixed path/busid strings and require valid termination/zero-filled output without reading past the field.

CMD_SUBMIT carries OUT payload; RET_SUBMIT carries IN payload. Validate response lengths against requested buffer space and preserve short transfers. Derive ISO payload/descriptor placement, offsets, actual lengths and per-packet statuses from L1, using checked addition/multiplication. Explicitly handle the documented non-ISO packet-count sentinel rather than interpreting it as a huge allocation. Validate endpoint, direction, device correlation, flags, sequence and phase before submitting to an OS API. Maintain connection-local sequence ownership; the unlink request has its own sequence plus target sequence. Successful unlink suppresses a later submit response for the canceled target; too-late unlink is a distinct result. Do not extend standard headers for identity, reset, leases, events or encryption. Device-wide reset semantics require a backend/control design; do not invent an extra USB/IP data command.

## Independent case catalogue

| ID | Input / action | Expected evidence |
| --- | --- | --- |
| P01 | Independently encode DEVLIST with zero, one and several devices/interfaces; import success and failure | Byte-offset checks against L1 and independent peer decode; no leaked hidden device |
| P02 | Split each header/body at every byte; merge several messages into one read; short writes | Same outcomes as contiguous input, finite EOF cleanup at every split |
| P03 | Wrong version, opcode, direction, endpoint, phase; negative/oversize lengths; overflowing count products; malformed descriptors/strings | Reject before allocation or OS call; bounded memory and no panic |
| P04 | Control IN/OUT, zero length, endpoint halt/clear; bulk/interrupt IN/OUT, short packet and stall | Exact setup/data/status preservation; class/vendor requests remain opaque |
| P05 | Duplicate live seqnum, out-of-order completion, wrapped counter, stale device generation | Correct correlation or explicit session error; no completion delivered to wrong request |
| P06 | Unlink before completion, simultaneous race, completion committed first, missing target, repeated cancel | L1 wire outcomes; one terminal wire decision per submit, separately retiring backend token |
| P07 | ISO packet counts/offsets/statuses, empty and short packets, unsupported backend | Preserve valid per-packet results; reject unsupported capability before claiming support |
| P08 | SET_CONFIGURATION/SET_INTERFACE, endpoint reconstruction, reset/hotplug | No stale endpoint handles; native descriptors/driver binding match current generation |
| P09 | Slow receiver, backend stalls, disconnect at each state, malformed peer flooding | Credits remain within budgets; independent cancellation capacity; release by deadline |
| P10 | Unauthorized list/import/event subscription; revoke between list and import; guessed busid | No unauthorized descriptors or claims; identical policy on all tunnel ingress paths |
| P11 | TLS wrong/revoked identity, replay capability, downgrade, stale control connection | Fail closed; no raw USB/IP downgrade; no credentials in diagnostics |
| P12 | Multi-device attach failure on each step; heartbeat expiry; service restart | Per-device compensation/journal evidence and explicit quarantine on failed cleanup |

Fuzz codec and control parsers independently; property tests cover round trips, allocation budgets and state transitions, but self-round-trip alone is insufficient. Model cancel/complete/disconnect interleavings before hardware stress. No unsafe parser casts over network memory. Independently authored synthetic descriptors use authorized lab IDs or device metadata, never another project's fixtures.

## External peer and device matrix

| Pairing / purpose | Apparatus and scope | Required result |
| --- | --- | --- |
| KnightBus exporter → stock Linux importer | Named distro/kernel/config, `vhci_hcd`, exact usbip tool package/build, disposable board | DEVLIST/IMPORT and control/bulk/interrupt/unlink; isolated raw lab path and authenticated tunnel separately |
| Stock Linux exporter → KnightBus importer | Named Linux usbip-host setup and original M1 driver | Native Windows descriptors and transactions; separates importer correctness from Mac access |
| KnightBus Mac exporter → KnightBus Windows importer | M1 and M2 originals; actual joystick plus control board | Mandatory M3 proof in [critical path](critical-path-test-plan.md) |
| KnightBus exporter → externally installed usbip-win2 | Candidate source pin in ledger; exact binary version/signature/hash must be recorded at execution | Optional diagnostic oracle only; never substitutes for original importer gate |
| External Extender/sing-usbip/usbipd-mac ↔ compatible KnightBus role | Exact immutable build and supported device/path, after source audit gaps close | Optional differential behavior, not normative truth; account for sing extension importer requirement |
| usbredir, wolfusb, berndverst | Different protocol | Excluded as USB/IP wire peers; lifecycle ideas only |
| NKS | Proprietary | No source/binary test in this research plan |

Device coverage: known vendor test board for control and bulk IN/OUT; actual HOTAS for control/interrupt and composite interfaces; a device exposing interrupt OUT if HOTAS lacks it; later dedicated audio/iso device for packet scheduling; disposable read-only storage case only where useful. Never infer all transfer types from one device. Linux may run in a VM for network protocol tests but that does not validate Windows x64 kernel behavior on an Apple Silicon host.

## Reproduction and evidence format

Each future run records experiment ID, UTC time, KnightBus commit and hashes, peer commit/binary hash/package version, OS build/architecture/kernel configuration, driver package/version, device topology and redacted descriptors, security settings, precise commands, expected outcome, observed statuses, logs and result. Record individual directions/types as TESTED / API-supported-but-untested / unsupported / UNKNOWN. Keep PASS / CONDITIONAL / BLOCKED / UNPROVEN for gate verdicts.

Linux apparatus commands to execute later include `uname -a`, `usbip version`, `modinfo vhci_hcd`, `usbip list -r <lab-exporter>`, `usbip attach -r <lab-exporter> -b <approved-busid>`, `usbip port`, and `usbip detach -p <owned-port>`. Binding and module-loading are separate controlled setup operations. These commands were **not executed here**. Verify exact distro syntax before use. Capture packet header/timing metadata only; a full network capture can contain private USB traffic. Sanitize any retained fixtures and keep raw lab evidence access-controlled.
