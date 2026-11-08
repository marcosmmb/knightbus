# KnightBus Milestone 0: reference audit and evidence ledger

Audit date: 2026-11-08. Research and design only. **Milestone 0 acceptance remains UNPROVEN:** seven artifacts are supplied, but the requirement for seven source-level reviews tied to exact commits is not met by the available remote access. Do not advance this report to PASS until the acquisition gaps below are closed. Milestones 1, 2 and 3 have no KnightBus execution evidence.

## Baseline and method

The workspace contained only `.git`, no commits, code, tests, previous milestone evidence, research documents, AGENTS.md, or approved ADRs. The requested plan was read from `/Users/marcos/Downloads/KnightBus_Development_Plan_Risk_First.md`; SHA-256 `6ca3affe067e3e591bf0d7a76941e6d2531ed9d74d0dd8f7e579c11012147b08`. Its requirements and the user's instructions are the authority for this work. Proposed choices here are not approved ADRs.

Evidence classes:

- **SPEC**: official protocol or OS API documentation; establishes a contract, not working KnightBus hardware.
- **STRUCTURE**: source or manifest actually read; scope and revision limitations stated below. Directory names alone are inventory evidence.
- **CLAIM**: upstream README, release note, or upstream-reported experiment/log. Even an upstream log is not an independent KnightBus reproduction.
- **TESTED**: independently executed behavior with apparatus, commands and outputs. No upstream build, protocol peer, USB hardware, driver or signing experiment was executed in this milestone.
- **UNKNOWN**: missing access or evidence. A planned test is UNKNOWN, not TESTED.

Direct `curl -L --fail --silent --show-error https://api.github.com/repos/calibrae/extender/commits/HEAD` failed with exit 6 (DNS resolution). An escalated retry was rejected by the environment's approval policy before execution. The web tool supplied cached GitHub pages, but many history, immutable-source and raw-source requests returned cache misses. No repository was cloned or forwarding source/binary stored in KnightBus. Cached branch pages can be from different dates; they cannot be attributed to a separately resolved commit without comparison. No claim of current HEAD is made.

## Revision register

These exact SHA values came from GitHub commit-link targets on history/release pages. A resolved SHA is a **candidate audit pin**, not evidence that subsequently read branch content matches it. Immutable file retrieval failed for the attempted pins. Missing pins are an explicit outstanding Milestone 0 requirement, not placeholders for fabricated values.

| Repository and metadata source | Candidate audit pin | Commit association / review gap |
| --- | --- | --- |
| [calibrae/extender](https://github.com/calibrae/extender/releases) | `21d622a8d03d194a80ea27c1c5beb0e7678ff1c6` | v0.3.0 release link; source reviewed at unpinned main only |
| [SagerNet/sing-usbip](https://github.com/SagerNet/sing-usbip/commits/main/) | `28bd42667ecac597d3e19492b9e81ee04e22792f` | History entry 2026-08-17; source reviewed at unpinned main |
| [beriberikix/usbipd-mac](https://github.com/beriberikix/usbipd-mac/releases) | `1069f3b84a190530b54e9b2aee4c219003e6e8c9` | v0.6.0 release link; main README discusses later 0.7.0 removal; do not conflate |
| [otti83/airusb-hub](https://github.com/otti83/airusb-hub) | **UNRESOLVED** | README retrieved; history/source acquisition failed |
| [vadimgrn/usbip-win2](https://github.com/vadimgrn/usbip-win2/commits/master/) | `83bd1f781d57ed6efdf15530c55710cf5d4482bc` | History entry 2026-08-23; branch source may be newer |
| [dorssel/usbipd-win](https://github.com/dorssel/usbipd-win/commits/master/) | `846f9227b7f29b8aa1513ca0a7ad3f5afe42162e` | Cached history 2026-04-08; branch source may be newer |
| [berndverst/usb-over-ip](https://github.com/berndverst/usb-over-ip/commits/main/) | `ad6b1017936d5d9f425a5973f8d36de3bfc1ffdd` | History entry 2026-01-29; source reviewed at unpinned main |
| [SPICEorg/usbredir](https://github.com/SPICEorg/usbredir/commits/master/) | `bcb6db72d481965bcfdd890d338b54b0f502a486` | Mirror history 2014-07-18; not a statement about current canonical upstream |
| [intelligentwolf/wolfusb](https://github.com/intelligentwolf/wolfusb) | **UNRESOLVED** | README only; no implementation review |
| [00b7ce/lan-usb-bridge](https://github.com/00b7ce/lan-usb-bridge) | **UNRESOLVED** | README only; no implementation review |
| [ameggs/OpenUSBIP](https://github.com/ameggs/OpenUSBIP) | **UNRESOLVED** | README and dependency manifest, not runtime review |
| [nks-hub/nks-usbshare](https://github.com/nks-hub/nks-usbshare) | **UNRESOLVED** | Public README only by policy; do not resolve this gap by reading implementation |
| [microsoft/windows-drivers-rs](https://github.com/microsoft/windows-drivers-rs/commits/main/) | `36558802149bc92455bf5719fe77f3a829d8580f` | History entry 2026-08-31; README/build script at unpinned main |
| [torvalds/linux](https://github.com/torvalds/linux/commits/master/) | `827751b699b79a6e569983359c02dce67f81b94c` | History entry 2026-09-11; no implementation reviewed; protocol documentation read separately |

## High-priority findings

### Extender

**STRUCTURE, limited to manifest:** [extender/Cargo.toml](https://raw.githubusercontent.com/calibrae/extender/main/extender/Cargo.toml) declares protocol, API, exporter/importer-role, daemon and CLI packages; async, TLS and discovery dependencies. This confirms separation intent, not cancellation correctness. Manifest declares MIT OR Apache-2.0. **CLAIM:** [README](https://github.com/calibrae/extender#readme) describes Linux interoperability, Windows through usbip-win2 and macOS DriverKit deployment barriers. KnightBus must independently design its boundaries; neither the Windows dependency nor advertised macOS path proves our gates. Protocol/runtime source requests failed; review incomplete. No tests run.

### sing-usbip

**STRUCTURE:** [control_protocol.go](https://raw.githubusercontent.com/SagerNet/sing-usbip/main/control_protocol.go) has separately framed control messages, bounded payload lengths, snapshots and heartbeat messages. [client.go](https://github.com/SagerNet/sing-usbip/blob/main/client.go) has cancellation contexts, platform import-host construction, worker tracking and attach/retry cleanup. This does not prove bounded runtime queues or correct race handling. **CLAIM:** [README](https://github.com/SagerNet/sing-usbip#readme) lists Linux kernel backends, macOS IOUSBHost with SIP disabled, bundled Windows drivers, separate notification connections, and an importer requiring its extension. Research lesson: make KnightBus notifications optional and independently versioned; do not inherit its control framing, matching semantics or deployment shortcuts. Exact license text not retrieved; plan identifies GPL-3.0-or-later, unconfirmed here. No tests run, including listed Linux interop tests.

### usbipd-mac

**STRUCTURE, inventory/manifest only:** [Package.swift](https://github.com/beriberikix/usbipd-mac/blob/main/Package.swift) and [core directory](https://github.com/beriberikix/usbipd-mac/tree/main/Sources/USBIPDCore) show Swift targets and separate device, protocol, network and coordinator components; ownership implementation retrieval failed. **CLAIM:** [README](https://raw.githubusercontent.com/beriberikix/usbipd-mac/main/README.md) says claimable debug/serial devices work, HID/storage/audio/camera interfaces are refused, and old system-extension code was removed. [Entitlement report](https://raw.githubusercontent.com/beriberikix/usbipd-mac/main/Documentation/development/entitlement-validation.md) describes open/seize tests, entitlement denial and an ineffective registry-property claim strategy. That report contains older broad driver-ownership statements contradicted by the newer README's serial-adapter correction. Older release installation instructions also disagree with main. Treat each as versioned upstream claims, not universal OS facts. MIT indicated. KnightBus must compare legacy open/seize with Apple's separate device-capture API, test actual interfaces, and measure return to local ownership. No tests run.

### AirUSB Hub

**CLAIM only:** [README](https://github.com/otti83/airusb-hub#readme) reports one real SuperSpeed flash drive exported by macOS and enumerated by Linux, encrypted peer sessions, refusal of interrupt/isochronous endpoints, a Windows importer never loaded into a kernel, and a macOS importer entitlement barrier. It reports different access outcomes for a root daemon and console-session process. These claims motivate process-context tests; they do not prove joystick access or KnightBus signing. Apache-2.0 indicated. Source and pin acquisition incomplete. No tests run.

### usbip-win2

**STRUCTURE:** [network.cpp](https://raw.githubusercontent.com/vadimgrn/usbip-win2/master/drivers/ude/network.cpp) uses WSK buffers and kernel transfer-buffer handling; [driver inventory](https://github.com/vadimgrn/usbip-win2/tree/master/drivers/ude) separates virtual-controller, endpoint, request and networking concerns, with a separate filter directory. This supports a kernel networking observation, not proof of every transfer type or safe detach. **CLAIM:** Windows importer support and distribution described in [README](https://github.com/vadimgrn/usbip-win2#readme). BSD-2-Clause shown at repository level; per-file/release obligations still need review for external peer use. KnightBus chooses user-mode protocol/TLS and a new bounded IPC contract. No driver installed or tests run.

### usbipd-win

**STRUCTURE:** [AttachedClient.cs](https://raw.githubusercontent.com/dorssel/usbipd-win/master/Usbipd/AttachedClient.cs) correlates outstanding sequence numbers, serializes replies and arbitrates unlink against completion; its comments describe endpoint-wide cancellation limitations in its capture driver, and its reply channel is unbounded. These are reasons to test cancellation granularity and impose our own budgets. File marks GPL-3.0-only. **CLAIM:** [README](https://github.com/dorssel/usbipd-win#readme) describes Windows export to Linux/WSL, service installation and explicit sharing/binding. Neither binding nor admin privilege is importer identity authorization. No tests run.

### berndverst/usb-over-ip

**STRUCTURE:** [vusb_ioctl.c](https://github.com/berndverst/usb-over-ip/blob/main/driver/vusb_ioctl.c) contains WDF buffer retrieval, descriptor-length checks and virtual-device operations. That file alone proves neither working UdeCx enumeration nor a safe IPC implementation. **CLAIM:** [README](https://github.com/berndverst/usb-over-ip#readme) describes a custom VUSB protocol and KMDF design. It is not a USB/IP compatibility oracle. MIT indicated. Derive our IPC from official WDF ownership rules, using checked arithmetic rather than adopting this ABI or implementation. No tests run.

## Other references

| Reference | Actual evidence and limitation | Original requirement |
| --- | --- | --- |
| usbredir | STRUCTURE: [usbredirhost.h](https://raw.githubusercontent.com/SPICEorg/usbredir/master/usbredirhost/usbredirhost.h) describes transport callbacks, handle ownership and draining cancellation before close. Header LGPL-2.1-or-later; repository includes GPL utilities. Different protocol; no tests run. | Explicit callback lifetimes and teardown ownership; no borrowed API or packet format |
| wolfusb | CLAIM: [README](https://github.com/intelligentwolf/wolfusb#readme) describes Rust remote USB operations and multi-platform CLI. Custom protocol classification comes from the supplied plan; implementation not confirmed. MIT in plan, license unverified here. | Useful diagnostics distinguish remote operations from native OS attachment |
| LAN USB Bridge | CLAIM: [README](https://github.com/00b7ce/lan-usb-bridge#readme) reports Atom-S3 serial exchange via Pi/Windows 11 and usbip-win2 0.9.7.7, leases, rollback and privilege split; notes unauthenticated management, unencrypted data, in-memory state and collateral port detach. Apache-2.0 indicated; no tests run. | Authenticated durable cleanup and compensating multi-device acquisition; external tool orchestration excluded |
| OpenUSBIP | CLAIM: [README](https://github.com/ameggs/OpenUSBIP#readme) advertises mDNS and GUI attach/detach; STRUCTURE: [requirements.txt](https://raw.githubusercontent.com/ameggs/OpenUSBIP/main/requirements.txt) declares Qt and zeroconf. GPL-3.0 claim, exact text unverified; no tests run. | Discovery hints without authority; original Rust engine remains independent of UI |
| NKS | CLAIM: [public README](https://github.com/nks-hub/nks-usbshare#readme) advertises TLS, pairing, access controls and external virtual-controller attachment; distinguishes macOS remote access from whole-OS attach. Proprietary per user/plan; no implementation or binary inspected. | Independently defined per-importer permissions; no source-derived design |
| windows-drivers-rs | STRUCTURE: [wdk-sys/build.rs](https://raw.githubusercontent.com/microsoft/windows-drivers-rs/main/crates/wdk-sys/build.rs) generates bindings and feature-selects API subsets including USB. CLAIM: [README](https://github.com/microsoft/windows-drivers-rs#readme) explicitly cautions against production use. MIT OR Apache-2.0 declared. | Compile/link/load exact UdeCx API set before assuming Rust feasibility |

## Official documentation register

Accessed 2026-11-08; living documentation is not a Git commit pin. Record document snapshots/hashes with actual gate execution. Apple JavaScript pages and Markdown endpoints were partly unreadable; indexed official documentation supplied the device-capture and entitlement text. Do not treat inaccessible pages as reviewed API details.

| ID | Documentation | Basis and consequence |
| --- | --- | --- |
| L1 | [Linux USB/IP 1.1.1](https://www.kernel.org/doc/html/latest/usb/usbip_protocol.html) | SPEC: management exchange, framing, URB commands and unlink races; primary wire authority |
| W1 | [Microsoft UDE overview](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/developing-windows-drivers-for-emulated-usb-host-controllers-and-devices) | SPEC: KMDF client driver with UdeCx, WDK headers and Udecxstub.lib |
| W2 | [Write a UDE client driver](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/writing-a-ude-client-driver) | SPEC: controller/device/endpoint lifecycle tasks |
| W3 | [Endpoint queue](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/udecxusbendpoint/nf-udecxusbendpoint-udecxusbendpointsetwdfioqueue) | SPEC: attach WDF queue at PASSIVE_LEVEL |
| W4 | [Cancelable request](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdfrequest/nf-wdfrequest-wdfrequestmarkcancelableex) | SPEC: cancellation ownership and already-canceled return need handling |
| W5 | [URB completion](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/udecxurb/nf-udecxurb-udecxurbcompletewithntstatus) | SPEC: completion API, PASSIVE_LEVEL; do not generalize its IRQL to every UDE API |
| W6 | [Plug out and delete](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/udecxusbdevice/nf-udecxusbdevice-udecxusbdeviceplugoutanddelete) | SPEC: device handle cannot be reused after deletion |
| W7 | [Signing policy](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/kernel-mode-code-signing-policy--windows-vista-and-later-) | SPEC: production submission with HLK evidence; Windows-build-specific requirements must be rechecked |
| W8 | [Attestation](https://learn.microsoft.com/en-us/windows-hardware/drivers/dashboard/code-signing-attestation) | SPEC: registration/EV requirements and retail Windows Update restrictions; not KnightBus production approval |
| A1 | [Device capture](https://developer.apple.com/documentation/iousbhost/iousbhostobjectinitoptions/devicecapture?changes=_3___8&language=objc) | SPEC: terminate device clients/drivers; authorized entitled path or root; destruction resets and rematches interfaces |
| A2 | [VM device access](https://developer.apple.com/documentation/BundleResources/Entitlements/com.apple.vm.device-access) | SPEC: entitlement relates to USB capture for guest OS use; applicability to this product needs validation |
| A3 | [DriverKit entitlement requests](https://developer.apple.com/documentation/driverkit/requesting-entitlements-for-driverkit-development) | Official request route indexed; full page not retrieved; entitlement approval cannot be inferred |
| A4 | [USBDriverKit interface](https://developer.apple.com/documentation/usbdriverkit/iousbhostinterface) | Official interface reference indexed; not interchangeable with userspace IOUSBHost framework API |

## Acceptance audit and completion work

Seven artifacts exist; original requirements and experiments are specified. Source research is **incomplete**: four high-priority repositories have behavioral source observations (sing-usbip, usbip-win2, usbipd-win, berndverst); Extender has manifest-only review, usbipd-mac manifest/inventory review, AirUSB README only. None has a completed immutable-source association in this environment. Nine candidate pins were resolved, five references remain unpinned. No empirical capability has been demonstrated.

Before Milestone 0 acceptance: acquire exact commits outside the product tree in a permitted research environment; resolve the five missing hashes (NKS README/history metadata only); read the seven primary projects' relevant protocol/claim/driver lifecycle source at those commits; record file paths, commit dates, license text and test definitions without copying source or fixtures. Reconcile branch notes against immutable content. Inspect Linux USB/IP implementation only if needed to resolve SPEC ambiguities, at the registered pin or a newly recorded exact revision. Preserve this report's distinction between pin resolution, source review and execution. Review actual experiments before advancing any hardware gate. Moonlight/Sunshine source integration research is deferred to its later milestones.

## Executed documentation checks

`git diff --check` exited 0, but all new documents were untracked, so it does not establish their whitespace correctness. A separate Python integrity check passed: exactly seven expected artifacts, ten valid relative links, nine distinct 40-character candidate pins, five explicit missing-pin entries, balanced Markdown fences, final newlines and documentation-only non-Git inventory. A final Python check also verifies trailing whitespace and preserves the incomplete-evidence label. Its first two attempts failed because the embedded checker counted its own fence/marker literals; those checker errors were corrected, and the final run passed. No external URL reachability assertion is made by these checks. Exact final check:

```sh
python3 - <<'PY'
from pathlib import Path
import re
r = Path('docs/research')
names = {'reference-projects.md', 'idea-to-requirement-matrix.md',
         'platform-gaps.md', 'protocol-interop-test-plan.md',
         'license-boundaries.md', 'driver-feasibility-plan.md',
         'critical-path-test-plan.md'}
assert {p.name for p in r.iterdir()} == names
links = 0
for p in r.glob('*.md'):
    s = p.read_text()
    assert s.endswith('\n') and s.count(chr(96) * 3) % 2 == 0, p
    assert all(line == line.rstrip() for line in s.splitlines()), p
    for t in re.findall(r'\]\(([^)]+)\)', s):
        if not t.startswith(('https://', 'http://')):
            assert (p.parent / t.split('#')[0]).is_file(), (p, t)
            links += 1
s = (r / 'reference-projects.md').read_text()
assert len(set(re.findall(r'`([a-f0-9]{40})`', s))) == 9
assert s.count('**UN' + 'RESOLVED**') == 5
assert 'Milestone 0 acceptance remains UNPROVEN' in s
files = {str(p) for p in Path('.').rglob('*')
         if p.is_file() and '.git' not in p.parts}
assert files == {str(r / n) for n in names}
assert links == 10
print('PASS: documentation integrity; 7 artifacts, 10 links, 9 candidate pins, 5 pin gaps')
PY
```
