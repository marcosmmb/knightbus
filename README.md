# KnightBus

KnightBus is an original Rust USB forwarding project using USB/IP 1.1.1.
The **exporter** owns the physical device; the **importer** exposes it through
its native USB stack. The initial target is an Apple Silicon macOS exporter
and Windows x64 importer for a flight joystick.

**Milestone 4 contains architecture and decisions only.** At the owner's request,
workspace creation, code, tests and CI are deferred to their implementation
milestones. There is nothing to build or run in this branch.

## Design documents

- [Architecture and boundaries](docs/architecture.md)
- [Exporter/importer terminology](docs/terminology.md)
- [Embedded and Service hosting](docs/decisions/dual-mode-hosting.md)
- [Control API direction](docs/control-api.md)
- [Security and privileges](docs/security-model.md)
- [Measured gates](docs/gate-status.md) and [platform matrix](docs/platform-support-matrix.md)
- [Architecture decisions](docs/decisions/README.md)
- [Future testing strategy](docs/testing-strategy.md) and [M4 review](docs/milestone4-validation.md)

Both Embedded and Service modes will use the same engine. Service is the initial
packaging default on macOS and Windows. Neither mode bypasses native driver or
privilege requirements. The short M3 joystick experiment passed its hardware
scope; Windows framework-resource retirement and supported distribution remain
open. Historical findings stay on their original milestone branches.

M5 introduces the workspace, USB/IP codec and relevant CI. M6 owns engine/policy
implementation; later platform milestones own backends; M12 owns the full C/IPC
SDK. Review each implementation with its milestone's behavior and tests.

Original work is licensed under **MIT**;
see [LICENSE](LICENSE) and the [license decision](docs/decisions/license.md).
Read [CONTRIBUTING](CONTRIBUTING.md) and [SECURITY](SECURITY.md) before extending
this design. Old inspector and evidence directories remain excluded from Git.
