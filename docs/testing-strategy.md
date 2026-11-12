# Testing strategy

M4 reviewed architecture only. M5 now supplies the portable codec, fixtures, fuzz
targets and CI. [M5 validation](milestone5-validation.md) records executed checks.
M6 adds deterministic core and shared host-adapter tests; [M6 validation](milestone6-validation.md)
records their scope. The platform/security/SDK rows remain verification plans.

| Milestone | Required verification direction |
| --- | --- |
| M5: workspace and USB/IP codec | Formatting, Clippy, unit tests and Linux/macOS/Windows builds; clean checkout and selected MSRV; independent fixtures, fragmentation/coalescing, checked lengths, malformed fields and ISO structures |
| M6: engine and control | Authorization, leases, generations, bounded allocation, cancellation/completion ownership and cleanup; identical control scenarios through direct and IPC paths using mock backends and an injected clock |
| Platform backends | Native binding, real transfers, cancellation, callback retirement, detach/recovery and resource accounting on the exact supported OS/device builds |
| M11: security | Authentication, authorization, revocation, malformed traffic, replay, key handling and explicit rejection of downgrade |
| M12: C/IPC SDK | ABI layout and C/C++ consumers, handle/buffer/callback lifetime, version negotiation, authenticated IPC framing and real-device parity between both hosts |

M5 CI uses pinned reviewed actions, read-only permissions, finite
timeouts and no implicit publication or deployment. Add checks as their code
arrives. Portable builds must not require USB hardware, privileged capture or
native driver installation. Record actual compiler and platform versions.

M5's one-off Docker lab exercised stock Linux `vhci_hcd` with a synthetic device,
including all transfer types and cancellation. Its measured results are retained
in the validation report; the lab scripts are outside the repository and are not
part of CI. Keep reusable fixtures and fuzz targets beside the crate tests.

Hardware reports must distinguish generated fixtures, synthetic USB, physical USB
and native application use. Preserve before/after ownership, OS completion status
and local recovery. Do not infer successful cleanup from timeout, cleared internal
counters or driver rematching alone. Keep raw evidence local; committed reports
can contain sanitized findings and hashes.

[M4 review](milestone4-validation.md) records the earlier documentation-only scope;
[M5 validation](milestone5-validation.md) records the implemented codec checks.

M6 requires no additional CI job: the existing offline workspace checks include
all new core and adapter crates, with no new product dependencies.
