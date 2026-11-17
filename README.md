<p align="center">
  <img src="applications/knightbus-desktop/frontend/icon.svg" alt="KnightBus icon" width="112" height="112">
</p>

<h1 align="center">KnightBus</h1>

<p align="center">Share USB devices between your computers.</p>

<p align="center">
  <a href="https://github.com/marcosmmb/knightbus/actions/workflows/ci.yml?query=branch%3Amain">
    <img src="https://github.com/marcosmmb/knightbus/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI status on main">
  </a>
</p>

KnightBus lets one computer use a USB device plugged into another over your
network. Pair your computers, share a device, and connect to it from the other
computer. Your apps use their normal USB drivers.

## Features

- **One desktop app** to share local devices and connect to remote ones.
- **Nearby discovery and PIN pairing** to connect computers on your LAN.
- **Encrypted connections** with access granted separately for each device.
- **USB 1.x, USB 2 and SuperSpeed support**, including isochronous transfers.
- **Connection controls** to see active devices, disconnect, or stop sharing.
- **CLI and embedding APIs** for automation and use in other applications.

## Platform support

KnightBus is in early development. Current platform support:

| OS | Share local devices (export) | Use remote devices (import) |
| --- | :---: | :---: |
| macOS | ✅ | ❌ |
| Windows 11 x64 | ❌ | ✅ |
| Linux | ❌ | ❌ |
| Android | ❌ | ❌ |

✅ Supported · ❌ Not implemented yet

Windows packages include the Microsoft-signed **usbip-win2** driver.
Device compatibility depends on USB features
and OS permissions; see the [adapter limits](docs/architecture.md#native-adapters).

## Getting started

See the [Windows installation and pairing guide](tools/install/INSTALL.md) for
setup and connecting to a Mac. For the daemon and CLI, see
[standalone operation](docs/standalone.md).

To build from source, install [Rust](https://rustup.rs/) and your platform's C/C++
build tools. The repository pins its Rust toolchain. With GNU Make available:

```sh
make build                 # Build the daemon, CLI and libraries
make cli ARGS="local-list" # List local USB devices on macOS
make desktop               # Build and open the desktop app
```

The desktop app needs native webview dependencies. See
[Contributing](CONTRIBUTING.md#build-and-check) for prerequisites and test commands.
Without Make, build the workspace with `cargo build --workspace --locked`.

## Development

The Rust core handles USB/IP, device lifecycles and resource limits. OS adapters
handle native USB access. The desktop app, CLI and C API use the same core.

- [Contributing](CONTRIBUTING.md): build, test and add an OS adapter; `make help` lists shortcuts.
- [Architecture](docs/architecture.md): how the components fit together.
- [Control API](docs/control-api.md): manage devices and sessions from your own code.

## License

KnightBus is licensed under [MIT](LICENSE). The bundled Windows driver and other
third-party components retain their own licenses and notices.
