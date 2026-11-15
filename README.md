# KnightBus

KnightBus forwards USB devices between computers using USB/IP 1.1.1. The
**exporter** owns the physical device; the **importer** presents it to the remote
computer's USB stack so applications can use its normal device drivers.

The Rust implementation puts protocol handling, resource limits and device
lifecycle in shared crates. OS adapters supply native USB access.

## Supported path

The current adapters are a **macOS exporter** and a **Windows x64 importer**.
Windows uses the KnightBus kernel driver. Other OS directions are not implemented.

Control, bulk and interrupt transfers are supported, including USB 3.x device
speeds. Isochronous transfers, such as those used by many webcams and audio
devices, are not supported by the forwarding path. Device support also depends
on the adapter's descriptor and feature limits; see the
[architecture](docs/architecture.md#native-adapters).

The standalone programs require an authenticated tunnel supplied by the host;
the USB/IP transport itself does not authenticate or encrypt connections.

## Build

Install Rust using the pinned [toolchain](rust-toolchain.toml), then run:

```sh
cargo build --workspace --release --locked
```

Native adapters run on their respective OS. The Windows kernel driver is a
separate build; see [Contributing](CONTRIBUTING.md#windows-driver).
On macOS, list connected USB devices without capturing them:

```sh
target/release/knightbus-macos-exporter list
```

## Documentation

- [Architecture](docs/architecture.md): data flow, crate boundaries and native adapters.
- [Contributing](CONTRIBUTING.md): checks, implementation rules and adding an OS adapter.
- [MIT license](LICENSE). Third-party dependencies retain their own licenses.
