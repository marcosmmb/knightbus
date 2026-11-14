# Platform support and capability model

**M8 development scope PASS:** the exact Logitech joystick passes ordinary
explicit root capture, descriptors, interrupt input, errors/cancellation/reset,
active and idle unplug, in-process/temporary launchd hosting, process-death reclaim
and local HID input recovery. Unprivileged capture is correctly denied. The owner
deferred investigation of the earlier macOS 27.2.1 panic; no fix or production
approval is claimed. See the [M8 report](milestone8-validation.md) for evidence
and the remaining M9/M10/M12/release boundaries.

M5 implements the portable codec; M6 adds the portable engine and host APIs.
M7 adds a development Windows importer/UDE backend with integrated attachment-owned
controllers and passing native fault/isolation/final-reference retirement tests. The intermittent NCM stall
was traced to Linux's gadget timer; a temporary fixture correction passed repeated
traffic tests without changing KnightBus or stock `usbip_host`. M7 passes its agreed
fixture scope; physical-device and sleep/wake qualification were explicitly deferred
by the owner. Broader native/release qualification remains UNPROVEN. The historical table below
records the earlier baseline; see [M7 findings](research/windows-rust-driver-feasibility.md)
for current limits and measurements.

| Platform / role | Measured feasibility | Product backend / release | Hosting/default |
| --- | --- | --- | --- |
| Apple Silicon macOS exporter | PASS for root-captured `046d:c215` control/interrupt-IN primitive and M3 native path; M2 overall UNPROVEN | UNPROVEN: supported privileged service/helper installation, signing/notarization, fault coverage and classes | Service default; M8 root in-process and temporary launchd contexts PASS; ordinary-user capture Denied; M12 full parity unqualified |
| Windows 11 x64 importer | PASS for synthetic/native and real joystick uses; M1 overall UNPROVEN, queues retained | UNPROVEN: framework retirement, production signing, Secure Boot/HVCI, broad transfers | Service default; both hosts still need the same original native driver |
| Linux exporter/importer | M5 codec interoperability PASS with stock Linux importer and synthetic USB device; KnightBus native backend UNPROVEN | Not implemented; external kernel control/bulk/interrupt/ISO and cancellation checks pass within the documented lab scope | Service proposed; Embedded design retained |
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
