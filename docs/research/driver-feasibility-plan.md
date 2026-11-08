# Milestone 1: original Rust Windows UDE/KMDF feasibility gate

2026-11-08. **UNPROVEN; experiment not run.** This document defines a future disposable spike, not an approved production architecture. No Windows host, WDK build, driver installation, signing submission or kernel test was performed. Reference source pin/review gaps remain in [the ledger](reference-projects.md).

## Contract and apparatus

Use Microsoft's [UDE model](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/developing-windows-drivers-for-emulated-usb-host-controllers-and-devices): KMDF with UdeCx, official WDK headers and Udecxstub.lib. UMDF, WinUSB-only userspace and VHF joystick emulation do not establish this gate. [windows-drivers-rs](https://github.com/microsoft/windows-drivers-rs#readme) is experimental; a USB binding feature is not evidence of UdeCx ABI coverage or loadability.

Reserve a disposable Windows 11 **x64** machine/VM with a verified restore image, kernel debugger/WinDbg connectivity, crash dumps and a recovery console accessible without this controller. An ARM64 Windows VM on an Apple Silicon Mac cannot pass the x64 target gate. Confirm VM UDE support separately; repeat hardware-sensitive and release installation tests on a real x64 system. Do not bind or replace a physical host USB controller to install the synthetic root controller.

Record exact Windows build and patch level, hardware/VM hypervisor, x64 architecture, Secure Boot/HVCI/VBS states, Visual Studio Build Tools/MSVC, SDK/WDK/eWDK versions and hashes, KMDF target, Rust toolchain/target, LLVM/libclang, Cargo tooling and selected windows-drivers-rs immutable commit/crate versions. No toolchain version is asserted compatible before it builds. Freeze these in the M1 report. Developer test-signing belongs only in the disposable lab with restore instructions; production tests require normal security configuration.

## ABI and unsafe feasibility worksheet

Read [Write a UDE client driver](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/writing-a-ude-client-driver) and the exact DDI for every call. The following is a checklist to compile and verify, not a claim the Rust symbols already exist:

| Surface | Binding/link proof and ownership proof |
| --- | --- |
| Driver entry, WdfDriverCreate, WdfDeviceCreate and UdecxInitializeWdfDeviceInit / UdecxWdfDeviceAddUsbDeviceEmulation | Correct extern system ABI, WDF function table, object parenting, initialization order and Udecxstub linkage |
| UDE device initialization, descriptors, speed, create and plug-in | Descriptor storage lifetime, configuration consistency, device ownership; generated layout checked against WDK |
| Endpoint create and [queue assignment](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/udecxusbendpoint/nf-udecxusbendpoint-udecxusbendpointsetwdfioqueue) | Queue assignment requires PASSIVE_LEVEL; endpoint reset/purge/start callbacks need their own IRQL contracts |
| URB buffers and [completion](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/udecxurb/nf-udecxurb-udecxurbcompletewithntstatus) | Bounds, actual-length updates, correct USB/NT status mapping, no retained pointer after completion; this completion API requires PASSIVE_LEVEL |
| [WdfRequestMarkCancelableEx](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdfrequest/nf-wdfrequest-wdfrequestmarkcancelableex) and unmark/cancel callbacks | Handle already-canceled result, serialize completion ownership; no double completion |
| [PlugOutAndDelete](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/udecxusbdevice/nf-udecxusbdevice-udecxusbdeviceplugoutanddelete) | Handle is invalid after deletion; coordinate queue draining and callback retirement |
| Device interface, IOCTL queues, file close and service death | Service-only ACL, validated METHOD_BUFFERED copies, cancellation and generation invalidation |

Audit each callback for permitted IRQL, pageable/nonpaged memory, lock ordering, reentrancy, pending request references and cleanup parent. No async executor/TLS/USBIP parser in kernel. Use no_std where required, no unwinding across ABI, explicit allocation failure handling, and small documented unsafe wrappers. A Rust binding can be generated from official headers without translating an external driver. If a supported required API cannot be bound/linked from Rust, produce evidence and a proposed ADR; do not add C/C++ automatically.

## Original synthetic-device experiment

Prefer a small synthetic **CDC ACM** USB device with self-authored descriptors, intended to bind Windows's in-box usbser.sys. This tests native generic USB enumeration plus control/bulk and avoids a HID-only gate. Confirm the exact descriptor/class-binding requirements from Microsoft documentation during M1 before authoring the descriptors; failure to bind is a failed experiment, not justification to borrow descriptors. Use lawful lab identifiers; do not ship borrowed VID/PID. A self-authored generic USB HID or vendor test function can supplement interrupt coverage but cannot replace the native USB controller proof.

Proposed bounded IPC v1 for the spike: buffered IOCTLs for negotiate, create, dequeue request, complete request and remove. Each message has version, total length, device generation and opaque request ID; no user/kernel pointers or process virtual addresses cross the boundary. Cap descriptors at 64 KiB, 64 requests/device, 256 KiB/transfer, 8 MiB total buffered data; reject checked-arithmetic overflow, oversized completions, duplicate/stale IDs and invalid endpoint metadata before touching pending requests. These are experimental bounds, not final ABI constants. Keep a reserved cleanup path when data credits run out. Start with copies; shared-memory queues need a later measured justification.

UDE emits requests → driver retains owned WDF request safely → service dequeues bounded metadata/data → Rust harness responds → driver validates generation/length/status → completes original URB once. Delay a response intentionally and show the application request remains pending, proving it was not answered entirely from a kernel canned simulation. Then release it and record the real class-driver/application result. Descriptor enumeration handled locally by UDE is not sufficient evidence that EP0 traffic crossed userspace; select and trace an actual class control transaction as well.

| Test | Exact experiment | Pass threshold |
| --- | --- | --- |
| WIN-01 build/load | Original driver builds, INF/catalog verify, synthetic root devnode loads; debugger captures DriverEntry and UDE init | Reproducible clean build/load with full tool versions; no borrowed driver |
| WIN-02 native enumeration | Inspect device tree, descriptors and loaded class driver; open resulting COM port | Genuine USB child under original UDE controller and usbser binding; merely a named devnode fails |
| WIN-03 round trip | Class control operation plus at least 1,000 generated bulk loopback transactions through userspace | Every application result matches expected; request IDs and kernel/service timings correlate; both directions observed |
| WIN-04 cancel race | Delay response; cancel before dequeue, after dequeue and racing completion; send duplicate/late completions; 1,000 repetitions | One original completion, stale responses rejected, no leaked pending requests |
| WIN-05 faults | Kill harness with pending I/O; close service handle; restart driver; surprise detach; 100 attach/detach cycles | All requests terminate and virtual device disappears within 5 s cleanup target; memory counters return to baseline |
| WIN-06 adversarial IPC | Unauthorized open, malformed length/count, oversized response, wrong generation and queue exhaustion | No privileged operation for unauthorized caller; no kernel fault or buffer overwrite; bounded memory |
| WIN-07 verifier | Run targeted Driver Verifier on KnightBus driver, supported WDF checks, pool/IRQL tracking and debugger | No bugcheck/assert/leak in preceding workload; record actual enabled checks and duration |
| WIN-08 transfer matrix | Add original interrupt test function; inspect ISO API/WDK support; run dedicated packet experiment later | Control/bulk/interrupt/ISO individually labeled; unsupported or untested ISO cannot be marked passed |
| WIN-09 install/update/remove | Install A, update B with pending device, remove, reboot, reinstall; deliberately failed update rollback | Original system usable, no phantom controller/device, prior package restored on failed update |

No network or production service belongs in M1. Save build logs, package hashes, INF and catalog validation, SetupAPI logs, Device Manager/tree evidence, driver/module versions, transfer/cancel counters, verifier settings, debugger findings and exact commands as M1 artifacts. Expected spike directory: `spikes/windows-ude/`; it is intentionally absent in M0.

## Signing and reversible installation proof

[Microsoft signing policy](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/kernel-mode-code-signing-policy--windows-vista-and-later-) distinguishes production submissions with HLK evidence from test paths. [Attestation documentation](https://learn.microsoft.com/en-us/windows-hardware/drivers/dashboard/code-signing-attestation) describes Hardware Developer Program/EV requirements and retail Windows Update restrictions. Do not infer that Rust, UDE, a successful lab signature or another project's Microsoft-signed package establishes approval for KnightBus. Confirm current WHCP/HLK requirements for the selected Windows 11 build and HVCI compatibility; retain Partner Center eligibility and actual signed-package load evidence before a production PASS.

Future lab runbook: snapshot and record existing `pnputil /enum-drivers` state; validate the exact KnightBus INF/catalog with WDK tools; create only its root devnode with an appropriate documented device-install API/tool; record its instance ID and published `oemNN.inf`. Staging a package alone does not create a root devnode. Remove only that instance and owned package, stop only the KnightBus service, reboot if required, verify no residual device/service/package, then restore original security configuration and repeat. Never remove arbitrary `oem*.inf` or disable verifier globally on a shared machine. Record exact commands and identified paths in M1, not guessed destructive commands in M0. Recovery must be rehearsed before verifier/fault runs.

## Verdict rules and decisions

**PASS:** original Rust-first UDE enumerates with native class binding, demonstrated control + endpoint userspace round trip, cancel/fault/cleanup tests and reversible lab install. Report production signing separately as UNPROVEN until independently validated. **CONDITIONAL:** measured bounded exception with explicit owner-approved ADR. **BLOCKED:** reproduced supported-API/tooling failure with alternatives documented. **UNPROVEN:** no Windows host, incomplete test or source-only result. A shim proposal compares original minimal C bridge, different Rust ABI/boundary and deferred Windows release; implementation waits for explicit design approval. A third-party importer is never an alternative shipping implementation.
