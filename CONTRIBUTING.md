# Contributing

Read the [architecture](docs/architecture.md) before changing crate boundaries.
Keep changes focused and explain the resulting behavior and validation in the PR.
Bug reports should include the OS, device and steps to reproduce the problem.

## Build and check

The workspace pins Rust 1.85.0. From the repository root:

```sh
cargo fetch --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo test --workspace --all-targets --locked --offline
cargo test --workspace --release --locked --offline
cargo doc --workspace --no-deps --locked --offline
```

CI runs these checks on Linux, macOS and Windows with the pinned and stable Rust
toolchains, and treats documentation warnings as errors. Ordinary workspace tests
need no USB hardware or administrator access. Generated API docs describe the
individual types and methods; keep those comments beside the code.

## Implementation rules

- Put reusable protocol, scheduling and lifecycle behavior in the shared crates.
  Keep OS handles and API calls in native adapters.
- Prefer small public interfaces and short, clear functions and comments.
- Validate input and reserve resources before allocating or submitting work.
  Keep native buffers alive until the OS has finished using them.
- Cover behavior changes with unit or integration tests. Add a regression test
  for each bug fix, including failure and cancellation cases where relevant.
- Implement forwarding logic independently from specifications and OS contracts.
  Do not copy, translate or bundle another project's forwarding code or drivers.
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
`transport::exporter::serve`. Importers use `transport::importer::connect` to
validate descriptors, create the worker and forward requests using one budget.
Implement new cross-platform behavior in the shared contract and its tests first.

Test notification races, cancellation, removal and failed cleanup as well as normal
transfers. Portable regression tests belong with the crates.

## Windows driver

The kernel driver is outside the main workspace. Its build script requires Windows
x64, Visual Studio C++ tools, SDK/WDK 10.0.28000.0, LLVM and Rust 1.97.1. With those
installed, run from the repository root in PowerShell:

```powershell
cargo +1.97.1 fetch --manifest-path drivers/windows/Cargo.toml --locked
& ./drivers/windows/build.ps1
```

The script compiles, links and checks stack frame sizes. It does not sign, install
or load the driver. Running the Windows importer requires a matching signed driver
installed separately.

## Fuzzing

The USB/IP fuzz workspace tests whole-frame and incremental decoding. Its shared
assertions also run against synthetic fixtures in the ordinary test suite.
On Linux or macOS with a C++ compiler, run from the repository root:

```sh
rustup toolchain install nightly-2026-05-25 --profile minimal
cargo +1.85.0 install cargo-fuzz --version 0.13.1 --locked
cargo +nightly-2026-05-25 fetch --manifest-path crates/knightbus-usbip/tests/fuzz/Cargo.toml --locked
cargo test -p knightbus-usbip --locked --offline --test protocol write_fuzz_seeds -- --ignored
```

Then run each target (`decode` and `stream`) using:

```sh
ASAN_OPTIONS=quarantine_size_mb=32 CARGO_NET_OFFLINE=true cargo +nightly-2026-05-25 fuzz run --fuzz-dir crates/knightbus-usbip/tests/fuzz decode -- -max_total_time=45 -max_len=8192 -rss_limit_mb=512
```

Keep generated corpora and crash artifacts untracked. Minimize failures and add
regression tests. Contributions to KnightBus are licensed under [MIT](LICENSE);
third-party code retains its own license and notices.
