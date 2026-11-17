# Contributing

Read the [architecture](docs/architecture.md) before changing crate boundaries.
Keep changes focused and explain the resulting behavior and validation in the PR.
Bug reports should include the OS, device and steps to reproduce the problem.

## Build and check

Install Rust and native build tools: Xcode Command Line Tools on macOS, Visual
Studio C++ Build Tools on Windows, or a C/C++ toolchain on Linux. The repository
pins Rust 1.88.0. The desktop app uses macOS's system webview or WebView2 on
Windows; native desktop builds are checked on those two platforms.

The Makefile needs GNU Make and a POSIX shell. On Windows, use Git Bash with GNU
Make installed, or run the Cargo commands directly. From the repository root:

```sh
make help             # List commands
make check            # Workspace formatting, lint, tests and API docs
make desktop-check    # Desktop formatting, lint, tests and release build
make ui-setup         # Install UI test dependencies and Chromium
make ui-test          # Node and browser tests
make package-test     # Python packaging tests
```

UI tests use Node.js 22 and npm; packaging tests use Python 3. On Linux, install
Chromium's system dependencies with
`make ui-setup PLAYWRIGHT_INSTALL_ARGS="--with-deps chromium"` (this may need
administrator access). Windows installer recovery checks run with
`make windows-install-test`.

Use `make fmt` and `make desktop-fmt` to apply Rust formatting. Commands can be
overridden, for example `make check CARGO="cargo +stable"` or
`make package-test PYTHON=python`. Cargo honors `CARGO_TARGET_DIR`; the desktop
targets keep the app and its companion daemon in the same build directory.

CI runs workspace checks on Linux, macOS and Windows with the pinned and stable
Rust toolchains, and treats documentation warnings as errors. `make check` covers
one local toolchain and OS; desktop, UI, installer and fuzz checks are separate.
Ordinary workspace tests need no USB hardware or administrator access. Generated
API docs describe individual types and methods; keep those comments beside the code.

## Packaging

On macOS or Windows, `make package` builds the real app and daemon, then creates
a native package and ZIP under `lab/packages/`. It does not install them. Choose
a new destination for each build; the packager refuses to overwrite an existing
directory:

```sh
make package PACKAGE_OUTPUT=lab/packages/KnightBus-test
```

Packaging needs Python 3 and the platform's native build tools. Windows also needs
NSIS (`makensis`) and Windows SDK SignTool on PATH. Its signed usbip-win2 driver
is downloaded and verified automatically, with its license and matching source.
To use an already prepared driver package, pass
`PACKAGE_ARGS="--driver-package /path/to/driver"`.

macOS packages receive a local integrity signature; public distribution still
needs Developer ID signing and notarization. The Windows driver is signed, but
the generated KnightBus installer is not publisher-signed. Use a packaged app
and the [installation guide](tools/install/INSTALL.md) for native USB setup;
`make desktop` runs the development executable without installing it.

## Implementation rules

- Put reusable protocol, scheduling and lifecycle behavior in the shared crates.
  Keep OS handles and API calls in native adapters.
- Prefer small public interfaces and short, clear functions and comments.
- Validate input and reserve resources before allocating or submitting work.
  Keep native buffers alive until the OS has finished using them.
- Cover behavior changes with unit or integration tests. Add a regression test
  for each bug fix, including failure and cancellation cases where relevant.
- Implement forwarding logic independently from specifications and OS contracts.
  Do not copy or translate another project’s forwarding implementation. The
  Windows package uses the pinned, unmodified usbip-win2 UDE driver through its
  public device interface and private loopback USB/IP, with its license and corresponding source included.
  Review new dependencies, their licenses and build scripts.

## Adding an OS adapter

Implement [`core::exporter::Driver`](crates/knightbus-core/src/exporter.rs) for a
physical device, or [`core::importer::Driver`](crates/knightbus-core/src/importer.rs)
for a virtual device. These traits require capability limits, native operations,
notifications and retirement. Their API comments define ownership and wakeup rules.

Exporters must implement configuration selection and an `InterfaceDriver` owning
one interface's native handles. Core decodes configuration, alternate-setting and
clear-halt requests, validates their targets and drains the affected transfers.
These requests never reach `submit`. Alternate changes receive only the targeted
interface; preserve other interfaces and report native USB errors through
`ControlError`. Failed topology changes retire the device through the shared worker.

Compose an exporter worker inside `Engine::new` and serve connections with
`transport::host::Host`. Importers keep a `transport::host::Remote` control link
and use its authenticated stream with `transport::importer::connect` to
validate descriptors, create the worker and forward requests using one budget.
Implement new cross-platform behavior in the shared contract and its tests first.

Test notification races, cancellation, removal and failed cleanup as well as normal
transfers. Portable regression tests belong with the crates.

## Fuzzing

The fuzz workspace tests whole-frame USB/IP, incremental USB/IP and control decoding. Its shared
assertions also run against synthetic fixtures in the ordinary test suite.
On Linux or macOS with a C++ compiler, run from the repository root:

```sh
rustup toolchain install nightly-2026-05-25 --profile minimal
cargo +1.88.0 install cargo-fuzz --version 0.13.1 --locked
cargo +nightly-2026-05-25 fetch --manifest-path crates/knightbus-usbip/tests/fuzz/Cargo.toml --locked
cargo test -p knightbus-usbip --locked --offline --test protocol write_fuzz_seeds -- --ignored
```

Then run each target (`decode`, `stream` and `control`) using:

```sh
ASAN_OPTIONS=quarantine_size_mb=32 CARGO_NET_OFFLINE=true cargo +nightly-2026-05-25 fuzz run --fuzz-dir crates/knightbus-usbip/tests/fuzz decode -- -max_total_time=45 -max_len=8192 -rss_limit_mb=512
```

Keep generated corpora and crash artifacts untracked. Minimize failures and add
regression tests. Contributions to KnightBus are licensed under [MIT](LICENSE);
third-party code retains its own license and notices.
