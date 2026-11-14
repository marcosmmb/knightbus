# Windows driver build, lab tests and rollback

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
& drivers/windows/package.ps1 -CertificateThumbprint YOUR_EXISTING_LAB_CERT_THUMBPRINT
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
& drivers/windows/install.ps1 -Action Stage -Record C:\Lab\knightbus-install.json
# Run the tests, then remove only this recorded installation:
& drivers/windows/install.ps1 -Action Remove -Record C:\Lab\knightbus-install.json
```

The record directory must already exist. The default `Stage` action installs the
signed package without creating a persistent controller. Preserve its exact OEM
INF name and digest in the record. `Controller::owned()` provisions each attachment,
including the parent-checked virtual-hub filter. Remove reserves all 64 controller
slots and refuses active or quarantined owners; it then uninstalls only the
hash-verified package. Fixed software-controller records can be reused on restaging.

`-Action Install` remains an explicit diagnostic option for older tests requiring
one persistent root controller. Its record additionally identifies the exact root
node, hub and previous filters. Remove deletes only its KnightBus filter entry,
preserves other filters, and removes that recorded subtree/package. Do not use
this persistent-controller path as the normal attachment lifetime: it retains UDE
control queues across attachments. Native command failures preserve the record
for inspection; no wildcard removal, `/force`, or security-policy change is used.

If PowerShell policy prevents a reviewed local script, use a process-scoped
execution-policy override, not a machine policy change. No security-setting
change is needed for uninstallation. If Windows reports a reboot requirement or
removal fails, retain the record and investigate; do not silently recycle the
controller or install another package over a loaded image. The NCM failure in
this lab required a Windows restart before scoped removal could complete; retain
that as a failed teardown result even if rollback subsequently succeeds.

## Repeatable native tests

First run normal workspace checks. Native tests are ignored by default because
GitHub-hosted CI has no staged KnightBus driver. With the package staged, an
elevated lab shell and no active attachments, run the normal ownership path:

```powershell
cargo +1.85.0 test -p knightbus-windows-importer --test native native_owned_ -- --ignored --test-threads=1
cargo +1.85.0 test -p knightbus-windows-importer --lib native::lifetime::tests -- --ignored --test-threads=1
```

These cover repeated native CDC transfers, failed attachment, process/network loss,
simultaneous owners, and a native COM file held open after the importer exits.
The held-file regression checks that the kernel retains an unsignaled slot event
until the last client closes, then acknowledges destruction. It deliberately
expects cleanup failure rather than premature credit reuse when the file remains
open past the deadline. The slot test reserves all names briefly and must run
without other lab owners. For the lower-level `native_cdc_bulk`,
`native_channel_rejects`, `native_owner_exit`, and explicit two-controller tests,
use separately recorded `-Action Install` diagnostic controllers.

The reservation tests also impersonate a privilege-stripped token with the
Administrators SID disabled. It must be denied creation in the protected object
directory while it can occupy all 64 old global names with mutexes. After reverting
to the elevated token, all protected controller slots must remain available.
Use matched adapter/driver builds: the protected-namespace readiness IOCTL differs
from the earlier global-event contract.

The tests serialize access to the lab controller. The original local CDC exporter
proves Windows enumeration, 20 checked bulk loopbacks, normal removal, abrupt
socket shutdown and importer-process termination. It uses native `usbser` COM
handles, not a fake client that reads the private IOCTL stream. Owned-object
counters are necessary checks but do not detect the known UDE control-queue growth.

For endpoint-stall recovery, stage the small test-only
[`tests/winusb.inf`](../crates/importers/knightbus-windows-importer/tests/winusb.inf)
package in the isolated VM. It binds Microsoft's inbox WinUSB to synthetic
`1209:0004`; it contains no driver binary. Copy that INF to a separate lab folder,
generate its catalog with the installed `Inf2Cat /driver:LAB_FOLDER /os:10_NI_X64`,
and sign the catalog using the existing lab certificate (`signtool sign /fd SHA256
/sm /s My /sha1 THUMBPRINT LAB_FOLDER\KnightBusTestWinUsb.cat`). Verify its catalog
signature, stage with `pnputil /add-driver LAB_FOLDER\winusb.inf`, and record the
returned OEM name and INF hash. Never change certificate trust for this test.

```powershell
cargo +1.85.0 test -p knightbus-windows-importer --test native native_bulk_stall_reset -- --ignored --nocapture
```

The fixture stays halted until the actual remote CLEAR_FEATURE arrives. A direct
clear is rejected to check native control-error propagation; a subsequent pipe
reset succeeds only after the peer observes its clear, followed by
20 native WinUSB round trips and full controller retirement. Remove only the
recorded, hash-verified test package afterward with `pnputil /delete-driver
OEM_NAME /uninstall`. No physical device is needed for this regression.

The separately ignored `native_two_controllers_isolate_owner_exit` test requires
`KNIGHTBUS_TEST_CONTROLLERS` containing two exact instance IDs separated by `;`.
It exercises both native COM devices, terminates one owner, then proves the
surviving controller still transfers data. Create the extra lab devnode explicitly
using the same staged package; record its returned instance ID, bind its own
virtual-hub filter with the same parent validation, and remove that exact node
afterward. A controller without its filter deliberately rejects ATTACH. This test does not require
or perform automatic hub recycling.

For stock Linux interoperability, use an isolated kernel with `dummy_hcd`,
`libcomposite`, stock `usbip_host`/USB/IP tools and `iptables`. The original bounded
[CDC fixture](../crates/importers/knightbus-windows-importer/tests/linux_fixture.py) owns one named
configfs gadget, limits its raw TCP port to the specified Windows peer, echoes
CDC bytes, and removes its gadget/exporter/firewall rule on exit:

```sh
sudo python3 crates/importers/knightbus-windows-importer/tests/linux_fixture.py --peer WINDOWS_LAB_IP --composite
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
    cargo +1.85.0 test -p knightbus-windows-importer --test native native_owned_controller_retirement -- --ignored --nocapture
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
cargo run -p knightbus-windows-importer -- --insecure-lab LINUX_LAB_IP:PORT BUS_ID 30
```

It fetches real descriptors and forwards native requests for the requested short
run. Exercise controls during that window and verify local recovery after unbind.
The optional final argument is an exact controller instance ID. This is not a
production authenticated network listener or a general configuration UI.

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
