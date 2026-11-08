# Platform gaps and Milestone 2 experiment

2026-11-08. All KnightBus OS capability gates are **UNPROVEN**. Source and official documentation references use IDs from [reference-projects.md](reference-projects.md). Upstream demonstrations remain CLAIM, regardless of the presence of upstream logs.

| OS / role | Supported API direction | Known gap / exact evidence needed |
| --- | --- | --- |
| Windows 11 x64 importer | W1/W2: original KMDF driver with UdeCx; Rust userspace IPC | Rust binding/link/load, cancellation/IRQL correctness, genuine native USB transfer and production signing not demonstrated; M1 |
| Apple Silicon macOS exporter | Userspace IOUSBHost or legacy IOKit FFI; A1 capture path; possibly USBDriverKit | Actual joystick/model unknown; ordinary open, capture and process context must be measured independently; M2 |
| Linux importer | Stock `vhci_hcd`, privileged local attach coordination | Exact kernel/config/tool ABI and module availability, tunnel/socket integration, detach behavior; no tested distribution yet |
| Linux exporter | Original Rust physical API backend or explicit Linux facilities | Driver ownership/unbind/restore and privilege boundary need tests; do not wrap an external forwarding daemon as product |
| Windows exporter | Future supported physical USB access/binding backend | WinUSB/class-driver capture is device-dependent; reversible binding and composite-device semantics unproven |
| macOS importer | IOUSBHostControllerInterface research | Different capability from physical export; managed host-controller entitlement reported by upstream; no entitlement granted to KnightBus |
| Android exporter | Future USB Host APIs | User permission, lifecycle, transfer coverage and importer backend required; no support claim |

## macOS API distinctions that affect the target

Enumeration, descriptor reads, opening a device, claiming each interface, submitting endpoint traffic and returning local ownership are separate proofs. IOHID reports prove none of generic export. Legacy open/seize measurements from usbipd-mac cannot by themselves disprove IOUSBHost device capture. Apple A1 describes device capture terminating existing clients/drivers, with either root privileges or an authorized entitlement path, and rematching upon destruction. That is an API contract, not a guarantee that this joystick is capturable on the user's OS or that the entitlement is available for a network exporter.

AirUSB's one-drive report and usbipd-mac's storage/HID barriers conflict at the level of general claims. Do not choose the more optimistic claim: compare API, OS build, process/GUI session, device generation and captured interfaces. Ordinary driver attachment is insufficient to decide claimability, as usbipd-mac's updated serial-device claims show. USBDriverKit IOUSBHostInterface is also a different API context from user-mode IOUSBHost framework objects; do not mix their signatures or entitlement requirements.

App Sandbox `com.apple.security.device.usb` relaxes sandbox access; it does not establish ownership transfer. DriverKit transport entitlements, `com.apple.vm.device-access` capture authorization, and virtual host-controller entitlement are distinct. Upstream denial is evidence about that upstream request, not a denial for KnightBus or every possible narrowly scoped request. Entitlement presence in a signature is not proof of a valid provisioning profile or successful activation. No SIP/reduced-security bypass is an acceptable production conclusion.

## Exact Milestone 2 experiment: original Rust diagnostic only

Prerequisites: dedicated Mac with current backups and separate local input devices, actual joystick/HOTAS, and a disposable known request/response USB development board as non-HID control. Record Mac model, Apple Silicon generation, `sw_vers`, `uname -m`, SDK/Xcode/Rust versions, account session context, `csrutil status`, signature and effective entitlements. Capture VID/PID, speed, configurations, alternate settings, every interface owner and endpoint; redact serials from published evidence. Unknown hardware identity is a prerequisite to fill, not an assumption that a generic joystick is equivalent.

| Experiment | Procedure to implement in M2 | Acceptance / classification |
| --- | --- | --- |
| MAC-01 inventory | Enumerate with original Rust IOKit inspector; read device/configuration descriptors, traverse lengths safely and record IORegistry interface ownership | Validated complete descriptor topology; enumeration alone does not pass |
| MAC-02 ordinary access | Try documented device/interface opens on control board and joystick, as console user and root; close all acquired handles on each attempt | Record exact IOReturn and per-interface result, no owner-name heuristic |
| MAC-03 device capture | Use A1 capture path on selected device, root variant first; reacquire services invalidated by capture; test interface access in console and daemon/agent contexts independently | Generic endpoint operations after capture, SIP on; a capture success with unusable interfaces fails |
| MAC-04 authorized capture | If actually provisioned, test A2 + IOServiceAuthorize flow without root; record profile/signature/authorization result | UNKNOWN if entitlement not available; do not ad-hoc-sign managed keys and call that approval |
| MAC-05 real transfers | Read standard descriptors over EP0; execute safe documented class/vendor control IN/OUT; submit joystick interrupt IN; exercise output/feedback only if device supports it; board bulk IN/OUT with known generated data | Exact request sizes/status/counts and bidirectional useful action; no IOHID substitution |
| MAC-06 lifecycle | Hold interrupt IN pending, cancel it, unplug, release capture, crash process, sleep/wake, reconnect; repeat 100 claim/release cycles | Operations retire or time out within 5 s lab cleanup deadline; no callback after free; local driver/application works again |
| MAC-07 composite ownership | Inspect all joystick interfaces, configuration/alternate changes and concurrent local clients | No partial-interface claim advertised as whole-device export; reject unsupported composite ownership |
| MAC-08 distribution | Test signed/notarized install and uninstall under normal security; inspect required prompts/profile and privilege split | Separate deployment result; root-only lab success is not consumer-install proof |

Use only documented safe requests appropriate to each device; storage writes are unnecessary. Timestamp submit/completion/cancel locally; logs exclude payloads and serials. The diagnostic must use ordinary Rust OS bindings or independently authored wrappers. If SDK/tooling forces a non-Rust bridge, record a proposed ADR and await explicit design approval before adding it.

**PASS for actual joystick:** every required interface is owned through generic USB APIs, control and real interrupt traffic succeed, safe bidirectional behavior supported by its descriptors is demonstrated, cancel/release restores local use, and security context is recorded. **BLOCKED:** reproducible supported-path failure for a required interface with exact API/errors and exhausted documented alternatives. **CONDITIONAL:** bounded measured tradeoff with owner-approved ADR. **UNPROVEN:** no device, incomplete transfer test or unavailable entitlement. Report control-board and joystick outcomes separately; a board PASS cannot authorize changing product scope. This milestone has not been run.

## Release blockers distinct from lab feasibility

Windows signing/HLK, Secure Boot/HVCI compatibility, Apple capture distribution/entitlement authorization, generic interrupt/iso scheduling and crash-safe release remain open even after a minimum lab path works. Linux peers provide protocol evidence, not evidence about Windows UDE or Mac ownership. Cross-platform support must be indexed by OS build, architecture, role, device and transfer direction. None of those cells is currently marked supported by KnightBus.
