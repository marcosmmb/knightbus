# Deferred implementation decisions

M4 selects boundaries, not unsupported platform promises. These choices remain
open with explicit closure work; they do not authorize fallback behavior.

| Choice | Current direction / closure |
| --- | --- |
| Windows total framework-resource retirement | Investigate minimal lifecycle/other supported UdeCx builds in M1/M7; no memory-growth allowance or automatic recycle approved |
| Apple installation/entitlements | Measure supported signed helper/service/extension route in M2/M8; no SIP-off dependency |
| Microsoft production signing and Secure Boot/HVCI | Qualify actual driver/install/rollback and certification route in M7/M17 |
| Async runtime/transport implementation | Prefer ordinary Tokio/rustls after exact transitive/build/provider review; inject runtime/clock, M6/M11 |
| Local IPC/authentication implementation | OS-authenticated UDS/named pipes and bounded length-prefixed JSON selected; implement and adversarially test in M6/M12 |
| Full C ABI/event ownership | Fixed-width, versioned, explicit ownership design; full asynchronous SDK/headers and C++ adapter tests M12 |
| Pairing/key lifecycle/lease defaults | Explicit exporter enrollment/pinning; storage/revoke/rotation and measured expiry limits M11 |
| Descriptor/ISO/transfer limits | Bounded explicit errors; choose per tested backend and device, M5–M10; no universal full-USB claim |
| Persistent journal and multi-device rollback | Explicit recovery/generation design; format/crash consistency and compensation tests M6/M11 |
| Desktop framework and packaging | Tauri preferred, optional unprivileged control client; decide dependencies in M13 |
| Additional OS roles | Preserve backend interfaces; require separate measurements and matrix cells in later platform milestones |

The owner-approved M4 sequencing exception is in [m4-scope](m4-scope.md). Any
future change of primary target, native shim or production scope must have its
own explicit decision. An unproven capability must not become available through
an implementation default.
