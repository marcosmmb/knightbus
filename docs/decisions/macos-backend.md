# ADR: generic Rust macOS USB backend, explicit ownership

Date: 2026-11-10. Status: generic-USB direction accepted; supported distribution
and full backend feasibility remain UNPROVEN.

**2026-11-13 M8 update:** the owner explicitly deferred the earlier kernel-panic
investigation while completing M8. Restore explicit development capture without
a recorder dependency, retain authorization and bounded ownership, and complete
remaining hosting/lifecycle checks with durable evidence (now passed). Optional diagnostic
recording fails closed once requested. This decision does not attribute or fix
the panic, or approve consumer distribution. See the
[M8 report](../milestone8-validation.md) and [crash lab](../macos-crash-lab.md).

Continue original Rust bindings to documented Apple USB APIs. M2/M3 established
root IOUSBHost device capture, separate interface ownership, descriptor/control
requests, asynchronous interrupt input, abort/callback drain and release on the
actual Logitech HID device with SIP enabled. Ordinary-open and unprivileged
capture failed for that tested context. An IOHID observer proved local application
recovery only; it is not the forwarding backend.

The backend must discover actual topology/generation, claim only authorized
interfaces, retain buffers through terminal callbacks, and restore OS ownership
on release. Refuse unsupported/owned devices explicitly; no HID emulation or
silent alternate-device substitution. Encode capabilities by transfer direction,
interface, privileges and process context. Do not claim daemon/Embedded support
from a console root CLI test alone. M8 subsequently qualified both root
in-process and temporary system launchd contexts; ordinary-user capture returns
Denied. Service files must be outside privacy-protected user document directories.

A supported signed/notarized helper/service or authorized Apple integration path
still needs proof. Do not select DriverKit entitlements, require SIP disablement
or declare an ordinary app capture path available without evidence. If an
OS-mandated Swift/Objective-C shim becomes necessary, document and obtain explicit
approval first; none is approved now. M4 installs no helper/extension and performs
no physical capture. See [historical inputs](../gate-status.md).
