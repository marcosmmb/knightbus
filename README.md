# KnightBus

KnightBus is an original Rust USB forwarding project using USB/IP 1.1.1.
The **exporter** owns the physical device; the **importer** exposes it through
its native USB stack. The initial target is an Apple Silicon macOS exporter
and Windows x64 importer for a flight joystick.

## Build and verify

Rust 1.85.0 is the minimum and pinned formatting toolchain. The product crate has
no third-party dependencies; after toolchain installation these checks run offline:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo test --workspace --all-targets --locked --offline
cargo test --workspace --release --locked --offline
```

See the [wire codec/API](docs/protocol-usbip-1.1.1.md) and
[fuzz instructions](crates/knightbus-usbip/tests/fuzz/README.md). Tests and their
24 golden vectors live under `crates/knightbus-usbip/tests/`. Portable tests require
no USB hardware or elevation. One-off lab tools are kept outside the repository;
the validation report records the measured Linux interoperability results.

## Design documents

- [Architecture and boundaries](docs/architecture.md)
- [Exporter/importer terminology](docs/terminology.md)
- [Embedded and Service hosting](docs/decisions/dual-mode-hosting.md)
- [Control API direction](docs/control-api.md)
- [Security and privileges](docs/security-model.md)
- [Measured gates](docs/gate-status.md) and [platform matrix](docs/platform-support-matrix.md)
- [Architecture decisions](docs/decisions/README.md)
- [Testing strategy](docs/testing-strategy.md) and [M4 review](docs/milestone4-validation.md)

Original work is licensed under **MIT**;
see [LICENSE](LICENSE) and the [license decision](docs/decisions/license.md).
