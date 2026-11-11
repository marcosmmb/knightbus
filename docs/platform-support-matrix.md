# Platform support and capability model

**M4 contains documentation only.** No engine, backend or runtime capability API
is implemented. Future compilation of a workspace, GUI or ABI library will not
by itself constitute evidence of USB support.

| Platform / role | Measured feasibility | Product backend / release | Hosting/default |
| --- | --- | --- | --- |
| Apple Silicon macOS exporter | PASS for root-captured `046d:c215` control/interrupt-IN primitive and M3 native path; M2 overall UNPROVEN | UNPROVEN: supported privileged service/helper installation, signing/notarization, fault coverage and classes | Service default; actual daemon and Embedded process contexts UNPROVEN |
| Windows 11 x64 importer | PASS for synthetic/native and real joystick uses; M1 overall UNPROVEN, queues retained | UNPROVEN: framework retirement, production signing, Secure Boot/HVCI, broad transfers | Service default; both hosts still need the same original native driver |
| Linux exporter/importer | UNPROVEN in KnightBus | Not implemented; stock USB/IP is a future external interoperability peer | Service proposed; Embedded design retained |
| Windows exporter | UNPROVEN | Not implemented; capture/rebinding and restoration need separate proof | Service proposed |
| macOS importer | UNPROVEN | Not implemented; native virtual USB path/entitlements unproved | No supported package |
| Android exporter / other OS roles | UNPROVEN | Not implemented; API leaves room for additional backends | Platform decision deferred |

| Transfer or operation | Actual initial-device evidence | M4 implementation |
| --- | --- | --- |
| Control IN | PASS: actual physical descriptors/native requests | None |
| Control OUT | Request reached device; HID SET_IDLE STALL preserved; successful feedback UNPROVEN | None |
| Interrupt IN | PASS: actual 7-byte joystick reports consumed natively | None |
| Interrupt OUT | Device has no such endpoint; other devices UNPROVEN | None |
| Bulk IN/OUT | Synthetic M1 CDC works; physical cross-host bulk UNPROVEN | None |
| ISO IN/OUT | UNPROVEN end-to-end; rejected by M3 subset | None |
| Pending disconnect, owner exit and local recovery | PASS within short M3 cases after repair | None |
| Reset, alt settings, multiple devices, sleep/wake, endurance | UNPROVEN | None |

Runtime capabilities must identify role, transfer type **and direction**, device/
interface topology, speed, maximum request/aggregate bytes, outstanding count,
configuration/reset support, required privileges and supported host contexts.
They must distinguish unsupported, unproven and currently available. Availability
is the intersection of compiled backend support, current OS/process permissions,
device topology, resource limits and exporter authorization. A historical PASS
alone never makes a runtime capability available.

Future negotiated capabilities are filtered before reservation/import; reject an
unsupported endpoint or legal request exceeding local limits explicitly, without
truncation or silently dropping interfaces. Keep OS diagnostics sanitized and
separate from public stable error codes. Never infer high-speed capability from a
USB 2.0 descriptor or USB3 virtual root-port declaration.

No unapproved target deviation is introduced. See [gate status](gate-status.md)
for immutable reports and [M4 scope](decisions/m4-scope.md) for current authority.
