# Inputs and current gate status

M4 reads the committed findings on their original branches. It does not merge
spike code or raw evidence into the production workspace. These are historical
measured reports, not new M4 hardware runs.

| Milestone | Immutable input | Verdict and consequence |
| --- | --- | --- |
| M0 | `4d5dcb9` (base of this branch), `docs/research/` | UNPROVEN for full source/license audit; research requirements available, remaining audit gaps retained |
| M1 | `ec55581203c3409d7d3810557e364252b1ace899`, `docs/milestones/m1/README.md` | UNPROVEN overall: original Rust UDE/native CDC and IPC/cancellation worked; total framework queues did not retire |
| M2 | `2a1e3d3890b28a6a42429e7bea8763ff2ded6d95`, `docs/research/macos-usb-feasibility.md` | UNPROVEN overall: actual joystick root capture/control/interrupt/abort and 100 local cycles worked with SIP on; ordinary-open HID claim and unprivileged capture failed; distribution/fault scope open |
| M3 | `0dd7376effac895b83a3fd50a0fc610c65d1bb52`, `docs/research/critical-path-end-to-end-proof.md` | PASS for the short actual-joystick hardware experiment; initial-platform gate remains UNPROVEN |

Retrieve complete reports without changing branches:

```sh
git show ec55581203c3409d7d3810557e364252b1ace899:docs/milestones/m1/README.md
git show 2a1e3d3890b28a6a42429e7bea8763ff2ded6d95:docs/research/macos-usb-feasibility.md
git show 0dd7376effac895b83a3fd50a0fc610c65d1bb52:docs/research/critical-path-end-to-end-proof.md
git show 0dd7376effac895b83a3fd50a0fc610c65d1bb52:docs/decisions/initial-platform-go-no-go.md
```

A shallow single-branch checkout may not have these objects. The corresponding
[committed M1](https://github.com/marcosmmb/knightbus/blob/ec55581203c3409d7d3810557e364252b1ace899/docs/milestones/m1/README.md),
[M2](https://github.com/marcosmmb/knightbus/blob/2a1e3d3890b28a6a42429e7bea8763ff2ded6d95/docs/research/macos-usb-feasibility.md)
and [M3](https://github.com/marcosmmb/knightbus/blob/0dd7376effac895b83a3fd50a0fc610c65d1bb52/docs/research/critical-path-end-to-end-proof.md)
reports identify the same inputs. Their old spike/evidence links may be absent by
the owner's explicit retention choice; a source digest is not a shipped evidence
archive. M4 introduces no build or dependency on those local artifacts.

## What was demonstrated

Logitech Extreme 3D Pro `046d:c215`, revision `5711`, full-speed, one HID interface,
interrupt IN `0x81`/7 bytes. Device/configuration/report descriptors were fetched
from the physical Mac device (18/34/122 bytes), registered with original UDE code,
and used by Windows HidUsb and a WinMM application. All four axes, hat and all
12 buttons were observed. This was generic USB transport, not IOHID emulation.

Mac: Apple Silicon `Mac16,10`, macOS 27.0 build `26A428`, SDK 27.0, SIP on, root
IOUSBHost device capture. Windows: WEASLEY x64 build `10.0.22631`, UdeCx
`10.0.22621.1`, KMDF 1.33/UDE 1.1, WDK 10.0.28000.0, test signing and Secure Boot
off in the lab. These exact reported versions do not imply support for other builds.

M3 demonstrated physical cancellation on disconnect and local Mac application
recovery. Owner process exit exposed a Windows cleanup bug; the repair stopped
new admission and completed owned requests before UDE removal. Reattach then
worked; three children and six owned endpoint queues retired. Total WDF queues
still grew **11 to 14** across those three attachments. This is not an approved
memory-growth bound, safe recycling strategy or acknowledged Microsoft bug.

The owner requested short testing rather than ten-minute endurance. Bulk and ISO,
other device classes, stock Linux interoperability, broad crash/cable/sleep/reset
coverage and production installation were not proved. M3's SSH/Python launcher
was temporary lab infrastructure, not the approved production transport.

## Authority to proceed

The owner's subsequent explicit request to fully implement M4 on `milestone4`
authorizes architecture work despite the earlier report's hold. The subsequent
scope correction defers all code, workspace creation and CI to later milestones.
[The M4 scope ADR](decisions/m4-scope.md) records that limited change. It does not
turn M1/M2 into PASS, approve a native shim, waive resource retention, change the
joystick/platform target or authorize distribution with weakened security.

M4 contains no package skeletons or executable code. Backend enablement and
release stay gated on measured lifecycle/resource safety, supported privileges,
Microsoft signing and Apple distribution. No CONDITIONAL production scope has
been approved. Architecture decisions below choose boundaries without promising
that unresolved platform support will become available.

M5 adds the portable codec and verification tooling. Its [validation report](milestone5-validation.md)
records passing portable checks and stock Linux kernel interoperability with an
original synthetic device, including all transfer types and cancellation. This
codec proof does not change the product platform gate verdicts above.

M7 **passes its agreed fixture-based development scope**. The owner explicitly
deferred physical-device and sleep/wake qualification for this milestone. The
original Windows importer and separately built UDE driver pass native CDC,
composite, HID and WinUSB fixture tests, process/network-loss cleanup and
independent-owner isolation. Fixed software-controller slots and a kernel-held
final-destruction event prevent resource reuse while a USB client still holds a
file, even after importer exit. Measured driver-tagged pool returned to zero
when the final client closed.

The intermittent network stall was captured in Linux's NCM gadget timer. A
controlled temporary correction to that test device passed repeated traffic and
fault checks with KnightBus and stock `usbip_host` unchanged; the stock gadget was
restored. Endpoint-halt clearing passes native error/reset/recovery checks.

Physical-device, sleep/wake, broader transfer and production-release qualification
remain separate gates. Physical bus reset is explicitly unsupported. See the
[M7 findings](research/windows-rust-driver-feasibility.md) for exact scope,
measurements and unsupported features. These results do not advance production
platform approval.
