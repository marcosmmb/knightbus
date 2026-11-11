# Testing strategy

M4 reviews architecture and documentation only. No Cargo workspace, implementation,
test harness or CI workflow is included. The following checks belong with the
code they validate; they are not claims of executed tests on this branch.

| Milestone | Required verification direction |
| --- | --- |
| M5: workspace and USB/IP codec | Formatting, Clippy, unit tests and Linux/macOS/Windows builds; clean checkout and selected MSRV; independent fixtures, fragmentation/coalescing, checked lengths, malformed fields and ISO structures |
| M6: engine and control | Authorization, leases, generations, bounded allocation, cancellation/completion ownership and cleanup; identical control scenarios through direct and IPC paths using mock backends and an injected clock |
| Platform backends | Native binding, real transfers, cancellation, callback retirement, detach/recovery and resource accounting on the exact supported OS/device builds |
| M11: security | Authentication, authorization, revocation, malformed traffic, replay, key handling and explicit rejection of downgrade |
| M12: C/IPC SDK | ABI layout and C/C++ consumers, handle/buffer/callback lifetime, version negotiation, authenticated IPC framing and real-device parity between both hosts |

Introduce CI in M5 with pinned reviewed actions, read-only permissions, finite
timeouts and no implicit publication or deployment. Add checks as their code
arrives. Portable builds must not require USB hardware, privileged capture or
native driver installation. Record actual compiler and platform versions.

Hardware reports must distinguish generated fixtures, synthetic USB, physical USB
and native application use. Preserve before/after ownership, OS completion status
and local recovery. Do not infer successful cleanup from timeout, cleared internal
counters or driver rematching alone. Keep raw evidence local; committed reports
can contain sanitized findings and hashes.

[M4 review](milestone4-validation.md) records the scope of this documentation change.
