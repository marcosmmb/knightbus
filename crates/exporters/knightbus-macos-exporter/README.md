# macOS physical USB exporter

The exporter uses Apple's IOUSBHost APIs to own a physical USB device and
forward its transfers through the USB/IP transport.

```sh
cargo build -p knightbus-macos-exporter --release --locked
cargo test -p knightbus-macos-exporter --locked --offline
target/release/knightbus-macos-exporter list
target/release/knightbus-macos-exporter inspect 0xCURRENT_REGISTRY_ID
```

`list` enumerates without capture. `inspect` opens the selected device for
standard descriptor reads. `serve` requires an explicit `--capture` argument
and an authorized physical generation. No command elevates itself.

The library exposes `native::enumerate`, `native::inspect`, and `native::Native`.
`Native` implements `knightbus_core::exporter::Driver`; the device worker lives
in `knightbus_core::exporter::Backend`. Construct the native owner inside the
worker factory so USB calls stay off the host/UI thread. The engine handles
authorization, leases, memory admission, request ordering, and retirement.
See the [adapter guide](../../../docs/native-adapters.md).

Workers wake on transfer completion, device removal, new work, and shutdown.
Failed abort/release APIs can require timed retries. Native buffers and device
ownership remain retained until callbacks retire; a cleanup deadline does not
permit early release. Reset or re-enumeration invalidates the physical generation
and requires rediscovery and authorization.

USB definitions live in `knightbus-usb`, USB/IP flags and statuses in
`knightbus-usbip`, and native resource limits in `src/constants.rs`.

Hardware fixtures and crash instrumentation are local-only files under the
repository-root, gitignored `lab/` directory. They are not part of this crate,
its installed binary, or its ordinary tests. The retained tool instructions are
in `lab/README.md`. Historical hardware results and the unresolved macOS panic
investigation remain documented in the [validation report](../../../docs/milestone8-validation.md).
