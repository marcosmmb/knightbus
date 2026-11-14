# Windows driver build, lab tests and rollback

Lab tools live under the gitignored repository-root `lab/` directory and are
absent from a fresh clone. The standalone `lab/Cargo.toml` depends directly on
the actual importer and shared crates. Driver helpers consume the real driver's
build output. Run all commands below from the repository root.

This is a development-only driver. Read the [M7 gate report](research/windows-rust-driver-feasibility.md)
before installation. Use an isolated Windows x64 test VM with a recoverable
snapshot and debugger; keep its management NIC independent of tested USB devices.
The package matches the synthetic hardware ID `ROOT\KNIGHTBUS_UDE`. The normal
importer owns a software controller and a filter on its own virtual root hub. Removing that
controller disconnects all virtual children on it; it should not rebind unrelated
physical hubs. That isolation still needs broad hardware qualification.

## Build and signing

Install Visual C++ Build Tools, SDK/WDK 10.0.28000.0 and LLVM 17. Install Rust
1.97.1 for the isolated kernel workspace; portable/userspace MSRV remains 1.85.0.
Fetch the locked kernel dependencies once with network access, then build offline:

```powershell
cargo +1.97.1 fetch --locked --manifest-path drivers/windows/Cargo.toml
& drivers/windows/build.ps1
& lab/windows/package.ps1 -CertificateThumbprint YOUR_EXISTING_LAB_CERT_THUMBPRINT `
  -DriverImage "$PWD\drivers\windows\target\x86_64-pc-windows-msvc\debug\knightbus_windows_driver.dll"
```

The build script configures the x64 WDK environment, uses the official UDE 1.1
stub, preserves symbols and checks original-function stack frames. Do not load
an unoptimized kernel build. The package script uses an existing private-key
certificate in `LocalMachine\My`, validates the INF, signs SYS/catalog and verifies
catalog members. It does not create trust, weaken boot policy or install anything.
Retain the exact signed image and matching PDB for each native run.

A lab certificate is not production signing. Microsoft's [dashboard requirements](https://learn.microsoft.com/en-us/windows-hardware/drivers/dashboard/code-signing-reqs)
require an associated valid EV certificate for attestation/WHCP submissions and
SHA-2 signatures. The supported release route still requires organizational
Partner Center access, an appropriate Microsoft submission, exact target-OS and
HLK/HVCI qualification, and testing on Secure Boot-enabled systems. None of those
release approvals has been obtained for KnightBus. Never instruct customers to
enable test signing or disable Secure Boot as a product installation step.

## Scoped installation and removal

From an elevated PowerShell on an already configured test-signing VM:

```powershell
& lab/windows/install.ps1 -Action Stage -Record lab/windows/target/installation.json
# Run the tests, then remove only this recorded installation:
& lab/windows/install.ps1 -Action Remove -Record lab/windows/target/installation.json
```

The record directory must already exist. The default `Stage` action installs the
signed package without creating a persistent controller. Preserve its exact OEM
INF name and digest in the record. `Controller::owned()` provisions each attachment,
including the parent-checked virtual-hub filter. Remove reserves all 64 controller
slots and refuses active or quarantined owners; it then uninstalls only the
hash-verified package. Fixed software-controller records can be reused on restaging.

The helper supports `Stage` and `Remove`. It no longer creates persistent
legacy controllers. Removal still accepts previously recorded legacy installs
so they can be rolled back using their exact node/package identities.

If PowerShell policy prevents a reviewed local script, use a process-scoped
execution-policy override, not a machine policy change. No security-setting
change is needed for uninstallation. If Windows reports a reboot requirement or
removal fails, retain the record and investigate; do not silently recycle the
controller or install another package over a loaded image. The NCM failure in
this lab required a Windows restart before scoped removal could complete; retain
that as a failed teardown result even if rollback subsequently succeeds.

## Repeatable native tests

The lab tests use the actual importer crate. For process-death checks, build and
select its real CLI explicitly. All native tests are ignored by default:

```powershell
cargo build -p knightbus-windows-importer --target-dir lab/product-target --locked --offline
cargo test --manifest-path lab/Cargo.toml --all-targets --no-run --locked --offline
$env:KNIGHTBUS_IMPORTER_BINARY=(Resolve-Path lab/product-target/debug/knightbus-windows-importer.exe).Path
```

With the matching driver staged, administrator access, and no active attachments:

```powershell
cargo test --manifest-path lab/Cargo.toml --test windows_native native_owned_ -- --ignored --test-threads=1
```

These tests cover failed attachment, repeated CDC transfers, process/network
loss, simultaneous controllers, and a COM client held open after importer exit.
The held-client test observes the kernel reservation event through the OS.
It does not depend on private importer modules or lab-only driver counters.

The SuperSpeed publication test is selected with
`native_superspeed_descriptor_publication_and_retirement`.
Legacy preinstalled-controller tests and private notification/slot tests were
retired with their implementation copies; historical results remain in the
validation reports. Future tests must target the actual public/OS contract.

For endpoint-stall recovery, stage the small test-only
[`winusb.inf`](../lab/windows/winusb.inf)
package in the isolated VM. It binds Microsoft's inbox WinUSB to synthetic
`1209:0004`; it contains no driver binary. Copy that INF to a separate lab folder,
generate its catalog with the installed `Inf2Cat /driver:LAB_FOLDER /os:10_NI_X64`,
and sign the catalog using the existing lab certificate (`signtool sign /fd SHA256
/sm /s My /sha1 THUMBPRINT LAB_FOLDER\KnightBusTestWinUsb.cat`). Verify its catalog
signature, stage with `pnputil /add-driver LAB_FOLDER\winusb.inf`, and record the
returned OEM name and INF hash. Never change certificate trust for this test.

```powershell
cargo test --manifest-path lab/Cargo.toml --test windows_native native_bulk_stall_reset -- --ignored --nocapture
```

The fixture stays halted until the actual remote CLEAR_FEATURE arrives. A direct
clear is rejected to check native control-error propagation; a subsequent pipe
reset succeeds only after the peer observes its clear, followed by
20 native WinUSB round trips and full controller retirement. Remove only the
recorded, hash-verified test package afterward with `pnputil /delete-driver
OEM_NAME /uninstall`. No physical device is needed for this regression.

For stock Linux interoperability, use an isolated kernel with `dummy_hcd`,
`libcomposite`, stock `usbip_host`/USB/IP tools and `iptables`. The original bounded
[CDC fixture](../lab/windows/linux_fixture.py) owns one named
configfs gadget, limits its raw TCP port to the specified Windows peer, echoes
CDC bytes, and removes its gadget/exporter/firewall rule on exit:

```sh
sudo python3 lab/windows/linux_fixture.py --peer WINDOWS_LAB_IP --composite
```

It prints the actual bus ID. `--composite` exposes two independent CDC functions;
omit it for the single-port test. It runs for 90 seconds by default (maximum 300),
and handles Ctrl-C/SIGTERM cleanup. A missing `dummy_hcd` requires a suitable lab
kernel/module; do not substitute `usbip_vudc` on this Ubuntu VM, whose shutdown
crashed during the recorded test. The [findings](research/windows-rust-driver-feasibility.md)
record the separately built, unmodified test module and source hash.

```powershell
$env:KNIGHTBUS_TEST_EXPORTER='LINUX_LAB_IP:43240'
$env:KNIGHTBUS_TEST_BUS_ID='BUS_ID_PRINTED_BY_FIXTURE'
$env:KNIGHTBUS_TEST_PORT_COUNT='2' # Use 1 without --composite.
try {
    cargo test --manifest-path lab/Cargo.toml --test windows_native native_owned_controller_retirement -- --ignored --nocapture
    if ($LASTEXITCODE -ne 0) { throw 'Native interoperability test failed' }
} finally {
    Remove-Item Env:KNIGHTBUS_TEST_EXPORTER,Env:KNIGHTBUS_TEST_BUS_ID,Env:KNIGHTBUS_TEST_PORT_COUNT
}
```

The [kernel gadget guide](https://www.kernel.org/doc/html/latest/usb/gadget_configfs.html)
and [stock USB/IP README](https://github.com/torvalds/linux/blob/master/tools/usb/usbip/README)
describe the external test interfaces. These fixtures do not prove physical-device
compatibility. The later NCM investigation and stock-kernel retest are documented in the
findings, including the captured Linux NCM timer failure and temporary fixture
correction. They are not a blanket
qualification of network devices.

For a physical device, export only the explicitly selected spare device with
stock `usbip bind`, use its actual bus ID, and drive it through its native Windows
application. The development runner is:

```powershell
cargo run -p knightbus-windows-importer -- --insecure LINUX_LAB_IP:PORT BUS_ID 30
```

It fetches real descriptors and forwards native requests for the requested short
run. Exercise controls during that window and verify local recovery after unbind.
The importer creates its own controller for each attachment. Raw USB/IP requires
an authenticated tunnel; this CLI is not a general configuration UI.

Before/after each lifecycle experiment, collect:

```text
!wdfkd.wdfdriverinfo KnightBusUde 0x1
!wdfkd.wdfdriverinfo KnightBusUde 0x271
```

Record total WDF objects, owned lifecycle counters, boot time, driver image hash,
class binding and native application results. Preserve dumps/PDBs if a failure
occurs. Do not enable Driver Verifier globally; a future verifier run must be
scoped to this exact driver with rollback and original settings recorded.

At the end of the Linux lab, stop only the recorded exporter/echo processes,
unbind the named gadget, remove its function symlink and configfs directories,
and delete the exact temporary firewall rule. For physical devices, `usbip
unbind` and confirm native driver recovery. Do not unload shared modules or remove
other gadgets. Remove the Windows installation using its recorded ownership.
