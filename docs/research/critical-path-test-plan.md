# Milestone 3: smallest original macOS-to-Windows hardware proof

2026-11-08. **UNPROVEN; no spike code or hardware execution.** Milestone 1 (Windows UDE) precedes Milestone 2 (actual macOS device access) in risk priority; both require measured evidence before this gate can pass. See [driver plan](driver-feasibility-plan.md), [macOS experiment](platform-gaps.md), [interop cases](protocol-interop-test-plan.md) and [original requirements](idea-to-requirement-matrix.md).

## Preconditions and apparatus

Close Milestone 0 immutable-source audit gaps before accepting the research gate. M1 must provide the original Rust-first UDE controller, bounded userspace request channel, native binding, cancellation and removal evidence. M2 must provide an original Rust generic USB access harness with measured claim/transfer/release on a named Apple Silicon/macOS build. Record Windows 11 x64 build/hardware, Mac build/SDK, device firmware/topology, LAN wiring/RTT and security settings. Use the actual HOTAS plus a disposable request/response test board; do not substitute a different joystick silently. Device model and lab machine availability remain unknown.

A control-board path can diagnose general forwarding while the joystick is blocked, but cannot pass the original joystick target. No external driver, IOHID-to-virtual-gamepad path or fixed synthetic descriptor set can satisfy this gate. All forwarding/driver logic must come from KnightBus spikes and documented OS APIs. Any non-Rust shim requires explicit prior design approval. Keep experiments in `spikes/`, not production crates, with no Moonlight/Sunshine integration.

## Minimum vertical slice design

Mac Rust harness acquires the selected physical device and validates its descriptors. Windows Rust harness obtains exporter-authorized metadata, creates a virtual device through the original UDE driver and forwards its actual request stream to the Mac. Implement only the independently authored USB/IP 1.1.1 management and control/bulk/interrupt/unlink subset required by those devices, derived from [L1](https://www.kernel.org/doc/html/latest/usb/usbip_protocol.html). Parse every fragmented read and partial write. Use physical configuration/interface/endpoint descriptors including class/vendor data; model Windows local address/configuration operations explicitly where the OS virtual bus requires local handling. Do not forward meaningless bus-local address changes blindly or disguise local-only synthesized descriptors as physical evidence.

Use a bounded Rust user-mode authenticated TLS tunnel around unchanged USB/IP. For this lab, exchange/pin generated certificates out of band and configure an exporter allowlist for exactly the lab importer and selected device generations. This is an explicit experimental setup, not finished pairing UX. Control metadata/events stay on a separate connection; no custom headers inserted into USB/IP. One stream per imported device; bind each stream to importer identity and lease. An optional isolated stock-peer raw listener must be explicitly enabled and cannot bypass the same allowlist. Unapproved devices cannot be listed/imported, even by knowing busid.

Apply experimental resource limits and deadlines from the matrix. If legal physical descriptors/transactions exceed them, fail clearly and document a deliberate revised lab limit; do not truncate. Reject ISO devices/endpoints from this minimum slice unless ISO is independently implemented and tested. Rejection is a capability gap, not a full-USB support claim.

## Execution and acceptance

| Test | Procedure to execute later | Required evidence |
| --- | --- | --- |
| HW-01 discovery/policy | Connect authorized importer; list/import explicitly enabled device; repeat with unpaired identity, wrong grant and guessed busid | Authorized inventory only, no physical claim on denied requests; revoke between list/import fails |
| HW-02 native enumeration | Fetch remote physical descriptors, create UDE device and inspect Windows native class/vendor binding | Match physical descriptor topology with privacy-safe digest; native driver and application open; no fixed synthetic substitution |
| HW-03 control | Execute GET_DESCRIPTOR and supported safe class/vendor control IN/OUT, including zero/short and stall behavior where apparatus permits | Matching setup/length/status at both ends, actual generic USB backend operations |
| HW-04 joystick | Enumerate actual HOTAS; operate every axis/button in a Windows application for 10 minutes; execute supported output/feedback operation or explicitly mark absent from device | Windows native USB driver consumes interrupt data; no IOHID emulation; composite interfaces accounted for |
| HW-05 bulk | On control board, run 1,000 bidirectional transactions using independently generated data, multiple lengths including short transfers | Match expected data digests, no payloads in production-style logs, no unexplained loss/status changes |
| HW-06 unlink/disconnect | Hold real IN transfer pending, unlink, remove cable/device, kill each harness independently, revoke policy and expire lease | No double completion; no stale generation completion; cancel/cleanup ≤5 s after declared failure/expiry; local Mac use restored |
| HW-07 repeated lifecycle | 100 import/use/detach cycles and a driver/service restart with pending requests | No phantom device or growing outstanding/pool allocations; local and remote recover predictably |
| HW-08 multi-device | Import board plus joystick, inject failure on second attach, then retry; detach one while the other runs | Compensate only resources owned by batch; preserve unrelated attachments or report bounded observed interruption |
| HW-09 interoperability isolation | Compare Mac exporter with stock Linux importer and Linux exporter with original Windows importer | Identifies which side violates SPEC; peer success never replaces original Mac→Windows proof |
| HW-10 measurement | Record 1,000+ local monotonic submit-to-completion samples per supported transfer class, idle and competing-LAN runs | Median/p95/p99/max, sample count, byte sizes, RTT, CPU/memory and queue depth; no cross-host subtraction without clock calibration |

The joystick minimum requires control and interrupt. Bulk is separately required on a suitable device. ISO remains API-supported-but-untested / unsupported / UNKNOWN until a dedicated transfer and scheduling experiment; M3 success must be scoped accordingly and cannot be advertised as universal full USB passthrough. Record sleep/wake, reset and alternate-setting behavior as tested or explicitly unresolved.

## Failure taxonomy and release gate

| Failure | Classify by evidence | Next action |
| --- | --- | --- |
| Required Mac interface cannot open/capture/transfer | MAC-CLAIM / entitlement / process-context | Exact APIs/IOReturn, OS and owners; return to M2; no HID-only fallback |
| UDE device fails enumeration/native binding | WIN-ENUM / descriptor / class-driver | Compare physical descriptors and OS USB trace; return to M1 if synthetic path also fails |
| Service request not completed/cancelled safely | WIN-IPC / lifetime / IRQL | Stop kernel testing, collect dump/counters, repair original IPC design |
| Invalid framing/unlink/length handling | USBIP | Reproduce P-series vectors and stock Linux peer comparison |
| Denied identity, stale lease, revoke race | AUTH/POLICY | Preserve fail-closed behavior; repair tunnel/policy without wire extension |
| Device stays claimed or virtual device remains | CLEANUP | Quarantine generation and report local recovery steps; cannot pass detach gate |
| Driver unsigned/test-only or capture install requires security reduction | DISTRIBUTION | Lab capability may be measured; production release remains blocked/unproven separately |
| No named hardware or missing evidence | APPARATUS | UNPROVEN, not BLOCKED API feasibility |

**PASS for initial target:** physical Mac-connected HOTAS claimed through generic USB, standard USB/IP requests completed through original Windows UDE with native binding/use, required transfer direction tests and bounded fault cleanup evidenced. **CONDITIONAL:** measured limitation with explicitly approved scope/ADR; never convert unknowns to conditional passes. **BLOCKED:** reproducible incompatibility with supported options and recorded alternatives. **UNPROVEN:** incomplete apparatus/run. Production driver signing, Apple distribution and all-transfer-type coverage remain separate verdicts.

Future report location: `docs/research/critical-path-hardware-proof.md` with raw evidence outside logs containing secrets/payloads. Include KnightBus and peer revisions, actual commands and results per HW/MAC/WIN ID, signature/provisioning/security evidence, native driver/device tree, counters and statistical inputs. Publish redacted evidence digests and clearly identify synthetic versus physical descriptors. This location is not created with empty success templates in M0.

## M0 validation and next work

M0 created exactly the seven Markdown research artifacts. No Cargo build/test, peer build, fuzz run, driver load, physical USB operation or signing test was run; no such test is claimed passed. Documentation integrity checks are recorded in the completion response after execution. The research gate remains incomplete because full immutable-source reviews/pins could not be acquired. Finish that audit first, then execute M1 on the dedicated Windows x64 lab, followed by M2 and this M3 proof. Production crate bootstrap remains deferred until measured gates and necessary design decisions are accepted.
