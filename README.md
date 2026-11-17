# KnightBus

KnightBus forwards USB devices between computers using USB/IP 1.1.1. The
**exporter** owns the physical device; the **importer** presents it to the remote
computer's USB stack so applications can use its normal device drivers.

The Rust implementation puts protocol handling, resource limits and device
lifecycle in shared crates. OS adapters supply native USB access.

## Supported path

The current adapters are a **macOS exporter** and a **Windows x64 importer**.
Windows uses the bundled, Microsoft-signed usbip-win2 UDE driver. Other OS directions are not implemented.

Control, bulk, interrupt and isochronous transfers are implemented, within the Windows adapter’s USB 1.x, USB 2 and SuperSpeed limits. Device support also depends
on the adapter's descriptor and feature limits; see the
[architecture](docs/architecture.md#native-adapters).

The standalone programs support mutually authenticated TLS 1.3. Peers must be
explicitly paired, and the exporter grants device access separately; see
[pairing and sharing](docs/architecture.md#pairing-and-sharing). Raw USB/IP
is an explicit loopback compatibility option requiring an authenticated tunnel.

## Build

Install Rust using the pinned [toolchain](rust-toolchain.toml), then run:

```sh
cargo build --workspace --release --locked
```

Native adapters run on their respective OS. The Windows driver is an unmodified upstream binary;
see [Contributing](CONTRIBUTING.md#windows-driver) for packaging.
On macOS, list connected USB devices without capturing them:

```sh
target/release/knightbus local-list
```

The standalone daemon, unified CLI and embedded C library use the same native
forwarding implementation. See [standalone operation](docs/standalone.md) for
pairing, sharing, native attachment, diagnostics and platform prerequisites.

The optional [Desktop Manager](tools/install/INSTALL.md) provides pairing, device
permissions, attachment and session diagnostics through the same daemon.

## Documentation

- [Architecture](docs/architecture.md): data flow, crate boundaries and native adapters.
- [Contributing](CONTRIBUTING.md): checks, implementation rules and adding an OS adapter.
- [MIT license](LICENSE). Third-party dependencies retain their own licenses.
