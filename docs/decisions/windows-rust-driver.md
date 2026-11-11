# ADR: original Rust UDE driver and userspace importer

Date: 2026-11-10. Status: architectural direction accepted; feasibility/release
remain UNPROVEN. No native shim or resource exception approved.

Choose a Rust user-mode importer that owns TLS, USB/IP parsing, sessions and
validation, connected through bounded versioned IOCTLs to an original Rust-first
UDE/KMDF virtual controller. Untrusted network parsing stays outside the kernel.
The driver publishes validated remote physical descriptors and native USB requests,
not a synthetic VHF/gamepad substitute. Use official WDK bindings/stubs as tooling,
not external forwarding drivers. Keep its workspace/profile/toolchain separate.

M1 demonstrated original synthetic CDC binding, user-mode control/bulk traffic and
cancellation; M3 demonstrated actual Logitech native HidUsb/WinMM input. M3 repaired
owner-exit cancellation and retired owned children/endpoint queues, but total WDF
queues still rose 11 to 14 after three attachments on UdeCx 10.0.22621.1. This
remains a gate, not a performance tradeoff accepted by architecture. Preserve
completion ownership, callback lifetime and measured IRQL; never delete internal
OS queues or add an extra delete after consumed UDE handles.

The lab used test signing/Secure Boot off. Supported Microsoft signing,
Secure Boot/HVCI, installation/rollback and resource retirement are independent
requirements. Do not enable production import or conceal automatic controller
recycling before those conditions are resolved. Compare supported OS/UdeCx builds
and isolate lifecycle behavior as [M1/M3 evidence](../gate-status.md) recommends.

If Rust tooling becomes demonstrably inadequate, propose an original minimal
native shim or different boundary with explicit approval and evidence. No current
result justifies importing usbip-win2 or another forwarding driver's code/binary.
