# Milestone 8 implementation and hardware validation

2026-11-13. **PASS for the owner-approved M8 development scope.** The owner explicitly
deferred investigation of the earlier kernel panic for M8 completion and requested
minimal manual assistance. The panic remains an unresolved known development risk;
there is no claim of a demonstrated fix or production release approval.

Normal explicit capture is restored without requiring lab infrastructure. The
backend still requires explicit host authorization and actual OS permissions.
Prepared diagnostic trials continue to require local full-sync and remote durable
acknowledgements; failure does not fall back to unrecorded execution. The historical
panic batch remains disabled. See the [crash lab](macos-crash-lab.md).

Current measured scope on the selected joystick:

| Check | Recorded result |
| --- | --- |
| Shared-engine capture/control/configuration/alternate/interrupt/release | 27 full cycles passed: 26 instrumented and one without native journaling; zero retained credits |
| Ordinary interface claim as root | Correctly refused, `0xe00002c9` |
| STALL and subsequent control recovery | Passed after fixing modern IOUSBHost STALL mapping |
| Native cancellation and explicit reset | Passed; canceled completion `-125`, reset retired the generation |
| Active-I/O physical hot-unplug | Passed after fixing disconnected Mach-port mapping; session closed, inventory removed, credits zero |
| Idle physical unplug | Passed via service-termination callback with no pending transfers; session closed, inventory removed, credits zero |
| Root in-process host and temporary system launchd host | Both passed using the shared engine/backend and `InProcessControl` |
| Ordinary-user capture | Correctly refused with `Denied` |
| Exporter SIGKILL and reclaim | New generation appeared; subsequent full engine cycle passed |
| Local application input after release/reclaim | Ordinary UID 501 IOHID consumer received three seven-byte reports after each host/reclaim trial |
| Panic reproduction/fix | Original cause unresolved; investigation explicitly deferred by owner for M8 completion |
| Bulk/ISO hardware, broad topology/OS coverage, production distribution | Unqualified; later hardware/release scope |

## Apparatus and initial measurements

- Apple Silicon Mac16,10; macOS **27.2.1 (26B131)**; SIP enabled. This differs
  from the M2/M3 build 26A428, so historical results do not qualify this build.
- Actual Logitech Extreme 3D Pro **046d:c215**, full speed, physical location
  `0x02120000`, one interface, interrupt IN `0x81`, seven-byte report.
- Current device/configuration descriptors were read through IOUSBHost: 18/34
  bytes. No HID emulation, third-party forwarding implementation or payload logging.
- Unprivileged ordinary interface claim failed with **0xe00002c9** and was
  reported as denied/not exportable. The backend never automatically escalates.
- A user-executed root probe completed five seven-byte interrupt reads, all
  status zero. Observed submit-to-completion times were 75,328; 98,421; 100,282;
  98,905; and 99,746 microseconds. These few first reads are not a latency benchmark.
- Immediate cancellation of the sixth request completed with normalized status
  **-125**, zero bytes, 1,431 microseconds. The probe reported zero retained
  transfer bytes/requests after release. Apple's HID driver rematched, and the
  registry ID changed from `0x100002dec` to `0x100002efa`.
- Local application use of every joystick control was not retested. Driver
  rematching is not proof of that behavior.

The first probe's temporary log was read before reboot; it no longer exists.
These results are transcribed from the observed tool output, not a retained
downloadable evidence archive.

## Panic evidence and limits of attribution

The user ran the prepared batch and reported a system panic. Its intended order
was ordinary root access, ten engine cycles, physical reset, creator crash and
reclaim, then a temporary root launchd job. **Do not mark any batch stage passed.**
The reboot removed `/private/tmp/knightbus-m8-lab.*`, including stage logs.

The OS retained `/Library/Logs/DiagnosticReports/panic-full-2026-11-13-223930.0002.panic`.
The exact panic, matching executable and source snapshot are preserved outside
the repository at `/Users/marcos/Documents/KnightBusLab/m8-panic-2026-11-13/`.
Full reports contain system process information and are not committed.

| Artifact | Identity |
| --- | --- |
| Panic SHA-256 | `1d016534c891e443f2f9c222cf97b4b0f70d12477f50316d06ef24b274779bdd` |
| Executable SHA-256 | `fe8a42aa71721eeb7bf0aa5d29ad76757c63d2045302f364deb2980f9d8ceadb` |
| Executable UUID | `9215E34D-D2D4-33BA-8E05-57129D10EFE8` |
| Matching installed kernel UUID | `88958656-FC2A-3A9B-83C5-B56B962EDC28` |

The panic is a kernel data abort with fault address `0x30`. Symbolication against
the matching installed kernel identifies **`IOEventSource::closeGate()+20`**.
The panicked `IOServiceTerminateThread` in `kernel_task` traverses IOHIDFamily and
`IOService::terminateWorker`. The exact IOHIDFamily routine is not symbolicated.

The panic's userspace stackshot includes KnightBus's `knightbus-macos-usb`
worker inside `IOUSBHostDevice::initWithIOService_options_queue_error_interestHandler`,
called from `Native::open`, the qualification factory and the bounded worker.
The main thread is waiting for engine acquisition. This establishes that capture
was in progress while the HID termination thread faulted. It does **not** establish
whether this was the first capture, a later cycle, a framework lifecycle race,
an application API misuse, or an independent Apple defect. There is no evidence
that the batch reached SET_CONFIGURATION, reset, crash or launchd tests.

At the initial triage checkpoint, no hardware replay, USB reset, capture, driver
change or security-policy change had been performed after the panic. The boot check confirmed the restart. No live
KnightBus process was found. The script's launchd stage is unverified; its
temporary `launchctl submit` job is not persistent across reboot.

## Instrumented replay: capture and release

The owner-executed single worker capture/release trial
`m8-1113-230012-03a5` completed with exit status **0**, without a reboot or new
panic report. Its 42 native journal records were all present in the retrieved
Linux mirror (147 total records including supervisor and kernel events).
The worker claimed interface 0 and pipe `0x81`, released ownership, observed a
new registry generation and returned memory credits to zero. This trial did
not submit endpoint transfers or request configuration/alternate changes.
Kernel logs show the old Apple HID driver stopping, capture release resetting
the device, and `AppleUserUSBHostHIDDevice` starting successfully afterward.

The archived executable SHA-256 is
`2d325647c0f95d7899cfdad1950dc467d15bf12132bdc67434920978cb72f9cb`,
UUID `D362E527-8D5C-353D-A9B2-0E2694ABBC95`. Evidence resides under
`/Users/marcos/Documents/KnightBusLab/m8-1113-230012-03a5/` and
`root@192.168.88.242:/var/tmp/knightbus-m8/m8-1113-230012-03a5/`.
The recovery collector retrieved the remote journal and found no new reports.
Synchronous local/remote journaling changes timing; this one successful cycle
does not establish a fix for the original panic or repeated-capture safety.

The subsequent single shared-engine trial `m8-1113-230438-291d` used the same
executable and also exited **0**, without a new panic report. All 74 native
records were present in the Linux mirror (180 records total). Device/report
descriptor reads returned 18/122 bytes; GET_CONFIGURATION returned one byte;
SET_INTERFACE(0) and SET_CONFIGURATION(1) succeeded; interrupt IN returned seven
bytes with status zero. The engine reported successful unlink and shutdown,
with zero retained credits. The unlink canceled a queued request before native
submission, so this run does **not** requalify native pipe abort. The initial
probe remains the only observed native-abort result. Configuration 1 and
alternate 0 were already selected; other configurations/alternates remain untested.
Evidence uses the same local/remote parent directories and this trial's run ID.

Trial `m8-1113-230654-51b1` then completed **three consecutive shared-engine
cycles**, each with successful descriptor/configuration/alternate/interrupt
checks, queued unlink, new generation and zero credits after shutdown. All 223
native records were recovered from Linux (496 total records); no new panic report
was found. The new kernel-stream startup gate was recorded before the child
started. No native backend change was made between these three replay trials.
This is four successful instrumented engine cycles in total, not a resolution
of the uninstrumented lifecycle panic.

The first error/recovery trial, `m8-1113-230930-eff4`, exited **1** at its first
check without a panic. The reserved-descriptor request returned raw IOUSBHost
status **`0xe0005000`**, but the backend mapped it to generic `-5` rather than
STALL `-32`. The installed SDK's `IOUSBHostFamilyDefinitions.h` identifies this
as `kUSBHostReturnPipeStalled`; the code handled only the legacy USB.h value
`0xe000404f`. Both codes now map to `-32`, with a regression test tied to the
observed callback. This is an error-reporting fix, not a fix for the kernel panic.
The failed run released ownership; all 42 native records were retrieved from
Linux (140 total), with no new panic report. Later fault checks did not run.
The failed binary SHA-256 is
`51b52a5566bad8c75c7195d5af2103d3ccc14aa59ae2624436382cecb024e447`.
A repeated invocation was refused by the exclusive `STARTED` marker before USB
access; future runners print that refusal directly instead of a traceback.

With the mapping fixed, `m8-1113-231150-514a` observed STALL `-32`, successful
18-byte descriptor recovery, successful clear-halt on the healthy interrupt pipe
and seven-byte input afterward. It exited 1 because the fixture incorrectly
required cancellation to win: the next seven-byte request completed normally.
The journal places its successful callback before `transfer.abort`, whose API
also returned success. This is a legitimate completion/cancellation race, not
evidence of failed retirement. The fixture now permits that outcome and makes
up to eight attempts, reporting native abort UNPROVEN unless it observes `-125`.
Reset was not reached in this run. Cleanup completed without a panic.

The revised trial `m8-1113-231411-f153` exited **0**. STALL/recovery, clear-halt
on the healthy interrupt pipe and subsequent input passed. Three cancellation
attempts completed normally; the fourth returned **-125, zero bytes**. Explicit
device reset succeeded, the old generation retired, a new generation appeared,
and retained bytes/requests returned to zero. All 79 native records were retrieved
from Linux (178 total); no new panic report was found. The executable SHA-256 is
`fa784ebdad47531878d96441c4e77ceb2f31466e451c02ae807351bccc29a48f`.
This qualifies the narrow recorded error/reset paths, not recovery from a
physically stalled data endpoint, unrelated device classes, or panic freedom.

The first hot-unplug trial `m8-1113-231652-d344` exited 1 after real physical
removal. Kernel logs report the hardware connection lost; the active request
completed with transaction error `0xe00002ed` (`-71`). The fixture rejected that
valid error before waiting for the independent removal notification. Its updated
version accepts this observed completion status, then still requires the engine
to close the session with `Unavailable`, remove inventory and release credits.
This failed run does not qualify removal cleanup; no backend behavior was changed
to conceal the transaction error.
Its 227 local native records included one final callback that did not reach the
live remote mirror before process exit (226 native records present remotely).
The collector now archives local log tails and recovery reports separately to
Linux and verifies their hash; this preserves evidence without pretending the
original live stream acknowledged the missing callback.

The second unplug trial, `m8-1113-231955-20b4`, preserved the transaction error
and then exposed a backend mapping omission: enqueue returned
**`0x10000003` (`MACH_SEND_INVALID_DEST`)** after the user-client port disappeared.
The backend mapped that to generic `-5`. This exact code now maps to device
unavailable (`-19`) and therefore retires the native generation through the
existing completion path; unrelated Mach errors retain generic failure. A
regression test preserves both this observed result and the preceding `-71`.
The failed run's recovery archive, including local callback tails, is verified
on Linux. Neither this correction nor the STALL mapping addresses the original
kernel panic.

With the disconnected-port mapping corrected, `m8-1113-232217-8768` passed
physical hot-unplug through the shared engine. The final requests returned
`-71` and `-19`; the session closed with `Unavailable`, discovery returned zero
devices, and retained bytes/requests returned to zero. The fixture consumed 38
completions after its initial report. All 161 native records were present in the
Linux mirror (194 total); the complete recovery archive was also hash-verified
there. No new panic report was found. The executable SHA-256 is
`4805f2419d995fbadabae5233a23bea06e81026d2cdcf65e192fa3237ed6806e`.
This is active-I/O removal; idle-only removal and sleep/wake remain unqualified.

Trial `m8-1113-232415-852a` then passed **ten consecutive engine cycles** with
the corrected backend, bringing the recorded engine-cycle total to fourteen.
All 743 native records were recovered from Linux (1,527 records total), and the
recovery archive hash matched remotely. No new panic report was found. This
matches the original batch's engine-cycle count, but omits its preceding ordinary
root access attempt and retains the timing-changing instrumentation.

Trial `m8-1113-232750-739e` restored that preceding ordinary root-access step.
It correctly reported **Denied, `0xe00002c9`**, then completed all ten engine
cycles. This brings the instrumented engine-cycle total to **24**. All 766 native
records were recovered from Linux (1,556 total), with a hash-verified recovery
archive and no new panic report. Its executable SHA-256 is
`5ee5b183d408e29773d8b1735e6c4ce98cc3395a0087e4af5078ef08bfd4fa87`.
This does not reproduce the original timing: each API boundary still waits for
local full-sync and a remote fsync acknowledgement.

The original panic/binary/source evidence and portable test logs are additionally
backed up as `m8-original-panic-and-tests-2328.tar.gz` under the local lab root and
`root@192.168.88.242:/var/tmp/knightbus-m8/`. Verified SHA-256:
`47de9cb6d9e81441dd4cd78a8c4268d915f705a0976269c77ae24e362155406d`.

## Implemented code, not hardware qualification

`crates/exporters/knightbus-macos-exporter/` contains original Rust coordination
with ordinary `objc2`/IOUSBHost bindings, not a Swift/Objective-C shim or a
third-party forwarding backend:

- Read-only enumeration, generation selectors, physical descriptor retrieval
  and bounded descriptor validation. Apple connection-speed enums are translated
  explicitly; VID/PID are metadata, not authorization credentials.
- Transactional ownership of all interfaces, actual GET_INTERFACE state,
  asynchronous control/bulk/interrupt requests and ASAP ISO transaction lists.
- One active native request per pipe to isolate pipe-wide aborts; independent
  pipes run concurrently. Pending, native and completed owners share 64 slots.
- Shared engine memory admission, callback retention and queue draining before
  buffer retirement; cancellation does not manufacture completion.
- Ordered configuration/alternate barriers, endpoint clear-stall API and explicit
  reset generation retirement. Reselecting configuration 1/alternate 0 passed
  the instrumented engine cycles; other alternates and recovery from a stalled
  data endpoint remain **UNPROVEN on hardware**. Explicit reset and clear-halt on a healthy
  pipe passed the recorded native-fault trial.
- Removal notifications to the shared engine, transactional drain and explicit
  refusal/quarantine semantics. Active-I/O physical hot-unplug passed after the
  disconnected-port mapping fix; idle removal subsequently passed via the native
  service-termination callback during finalization.
- Engine construction with a common backend/transfer budget. Existing backends
  retain the fixed-topology default; only opting-in backends validate live topology.

The adapter currently owns one selected physical device per backend instance.
The host must re-enumerate, construct a fresh backend and authorize the new
generation after capture release/reset. Multi-device aggregation and persistent
inventory orchestration are not supplied by this crate.

ISO accepts ASAP scheduling only. Importer-local absolute frame numbers have no
validated clock mapping. USB/IP flags outside declared support are rejected.
Bulk, ISO, SuperSpeed burst scheduling, nonzero alternates, device classes beyond
the joystick, sleep/wake and sustained workload coverage remain unqualified.

## Checks and remaining gate

Before the panic, the macOS workspace tests passed with the new worker tests.
Those regressions cover native-retirement retention, cancellation, queue limits,
generation rejection, independent pipes, topology ordering, idle removal and
cleanup retries. They do not prove Apple kernel safety.

After disabling capture, **83 debug tests and 84 release/doc tests passed**, with
zero failures and one intentionally ignored corpus-generation test in each run.
Clippy with warnings denied, formatting and diff whitespace checks passed.
At that historical checkpoint, three regression guards verified library/CLI
capture refusal and the disabled batch without invoking USB APIs. The current
guards instead verify explicit selector validation and budget admission without
a journal, while retaining the disabled historical batch. Final test logs are retained beside the panic
in the persistent lab directory. No Windows post-change qualification is claimed.
Apple's `container` preflight before hardware testing succeeded; it was not
involved in the panic batch. A later Linux verification attempt found its service
stopped (invalid XPC connection). `container system start` resolved that setup
issue. The cached `rust:1.85.0-slim-bookworm` image then passed **81 workspace
tests**, zero failures, one intentional ignored test, without warnings. Source
was mounted read-only, build output remained inside the removed container, and
the log is `linux-container-tests-2325.log` beside the panic evidence. No Docker
was used. The Linux lab itself lacked Cargo, so it was not used for this build.

During instrumented investigation, the exporter suite now passes **13 Rust
tests**, including real IOUSBHost STALL and disconnected-port regressions. The crash recorder
passes **three Python tests** (with failure-path subcases) covering token/ACK
validation, retained local records after remote refusal, and kernel-stream
readiness. Clippy with warnings denied, formatting and diff whitespace checks
passed for the updated exporter. These checks do not establish kernel safety.

The owner authorized continued reproduction on this development Mac after
reviewing the panic. The next experiment uses durable per-operation records
and a remote mirror, archived binaries/symbols/source, local HID lifecycle
observation and the documented recovery procedure.
Do not treat an arbitrary delay, a different thread, or the earlier single-probe
success as a demonstrated fix. Prepare an Apple issue with the matching panic
and a minimized API sequence if investigation supports it. The owner subsequently deferred this investigation for M8; explicit development
capture is enabled while the unresolved incident remains recorded.

Consumer helper installation, signing/notarization and entitlement authorization
remain separate release gates. Console root success does not establish Embedded
application or root-daemon support. Neither host mode should elevate the GUI or
weaken macOS security. No supported DriverKit entitlement has been obtained.


## M8 completion scope and final hosting/lifecycle trial

The final M8 checks covered ordinary explicit capture without recorder dependency,
actual root launchd loading of the shared engine, actual in-process adapter loading,
unprivileged refusal, process death/reclaim, local HID application recovery, and
idle physical removal. The new bounded finalization trial runs these in order,
removes its temporary launchd job, and requires just one physical unplug at its
readiness prompt. An independent ordinary-user IOHID consumer requires three
seven-byte reports before testing and after each release/reclaim. It never seizes
a device, modifies reports or participates in forwarding. This proves local input
recovery; all axes/buttons are not individually exercised in this M8 check.

Full bulk/ISO/composite/alternate hardware coverage and endurance belong to M10;
Windows end-to-end integration belongs to M9. M12 owns the production daemon/SDK,
full real-device IPC/Embedded parity, installer and performance comparisons. The
M8 backend implements generic requests and all selected API transfer types, but
does not label untested device/direction combinations hardware-qualified. Signed,
notarized distribution and the unresolved panic remain release concerns outside
the owner-approved M8 development completion scope.


### First finalization trial: normal capture and local recovery pass

`m8-1113-234306-3b2f` passed a root shared-engine cycle without native journal
initialization, routing authorization and attachment through `InProcessControl`.
Device/report reads, configuration/alternate reselection, interrupt input, queued
unlink and shutdown passed; owned credits returned to zero. An ordinary-user
capture attempt returned `Denied` (exit 1). An independent UID 501 IOHID consumer
received three seven-byte reports both before and after capture/release, proving
local application input recovery without requiring manual joystick movement.

The subsequent temporary root launchd job started but exited 1 while initializing
the journal: `Operation not permitted (os error 1)`, before any native USB journal
record. Its prepared recorder was under the developer's Documents directory. This
is a service filesystem-context failure, not evidence of USB capture denial in
launchd. The exact job was successfully booted out. The continuation stages the
fixed binary/recorder in a root-only directory under `/private/var/tmp`, and uses
an authenticated Unix socket outside Documents. Durable archives remain in the
lab directory and on Linux. No privacy permission or security setting is changed.
Process-death/reclaim and idle removal were not reached in this first trial.

Final portable checks: **86 macOS workspace tests passed; 81 Linux workspace tests
passed**, each with zero failures and one intentional ignored corpus test. Linux
used Apple's `container` CLI, without setup/execution errors in this final run.
Five Python recorder/orchestration tests pass, including partial-launchd failure
cleanup. Clippy for all workspace targets with warnings denied, formatting and
whitespace checks pass. Logs are `m8-final-macos-tests.log` and
`m8-final-linux-tests.log` under the private lab root.


### Final continuation: hosting and lifecycle pass

`m8-1113-234637-5db8` exited **0** with `m8_finalization=PASS`, using the same
Rust executable as the preceding trial. SHA-256:
`a6b9521ec36873a75efcc8b337fa86c48b71d451403619c01192a170dfe4bf8a`.

- **Root launchd:** a temporary system-domain `LaunchDaemon` completed a full
  shared-engine cycle and exited zero. Moving its fixed binary and recorder out
  of Documents resolved the pre-USB filesystem failure. The job was booted out
  successfully and its root-only staging directory was removed. This establishes
  basic development host loadability, not a shipped daemon/IPC implementation.
- **Exporter process death:** the supervisor killed only its own captured fixture
  with SIGKILL (exit -9). Registry identity changed from `0x100002039` to
  `0x100002059`; a fresh backend completed a full engine cycle and returned credits
  to zero. The interrupted process's input callback had completed before the kill,
  but its native owner/storage had not been released. This proves captured-owner
  death/reclaim, not every possible in-flight callback timing.
- **Local recovery:** an ordinary UID 501 IOHID consumer received three seven-byte
  input reports after the launchd release and again after owner-death reclaim.
  Together with the preceding normal-capture trial, local input recovery passed
  after all three host/reclaim paths. No axes/buttons were individually asserted.
- **Idle unplug:** after consuming one initial interrupt report, the fixture
  submitted no further requests. Physical removal delivered native message
  `0xe0000010` (service terminated). The engine closed the session with
  `Unavailable`, removed inventory and returned credits to zero; zero additional
  transfer completions were required. This independently qualifies idle removal.

All **227 native records** were present in the live Linux mirror (**583 records
in total**); the collector found no new panic reports. The complete recovery
archive was hash-verified on Linux:
`/var/tmp/knightbus-m8/m8-1113-234637-5db8/recovery-20261113-234803.tar.gz`, SHA-256
`07f04f342fd62cc92e56f3ce69bce47b6ef5aea71de705d3112531e8c9629b77`.
The previous finalization trial's recovery archive also matched remotely:
`recovery-20261113-234504.tar.gz`, SHA-256
`131baffac088cc3353961d079f7f256930090e0080aa31b7a1f10eebecf67478`.
Both reside under their run IDs in the local lab root and Linux evidence root.
The optimized release build passed. No Windows post-change or cross-host M9
qualification is claimed by these macOS/portable checks.

### Acceptance disposition

All four M8 acceptance criteria are met for the recorded development scope:
physical enumeration/claim/transfer on the exact Logitech Extreme 3D Pro;
explicit refusal of unavailable access; surfaced transfer errors, cancellation,
reset and active/idle disconnect; and a separately recorded joystick **PASS**.
The backend uses original Rust with ordinary Apple bindings, generic raw USB
requests, bounded async ownership and the same engine in both tested host contexts.
Build/development installation and privilege constraints are documented in the
[crate README](../crates/exporters/knightbus-macos-exporter/README.md).

The earlier kernel panic remains a known issue with its evidence retained. The
owner explicitly deferred its investigation; M8 completion does not mean the crash
is fixed, other hardware is qualified, or production distribution is approved.


Both finalization trials' SSH forwards and Linux recorder processes were stopped
by verified process identity after recovery backups completed. No temporary
launchd job, privileged helper, sudo rule or automatic hardware replay remains.
The retained evidence directories are preserved for future crash investigation.


## Post-acceptance exporter review (2026-11-14)

The worker now distinguishes retryable endpoint contention (`SubmitError::Busy`)
from terminal admission failure (`SubmitError::Failed`), including native scratch
memory exhaustion. The latter retires once with Linux `ENOMEM` (-12). A busy head
request blocks later requests on its endpoint/direction while independent pipes
continue. Admission ordinals, rather than USB/IP correlation sequence numbers,
preserve FIFO across slot reuse and sequence wraparound. Topology barriers also
wait for earlier queued requests, not only requests already running in the OS.

The worker immediately redispatches after progress and uses a latched condition
variable notification for new work, cancellation, completion and removal. Native
control/pipe/ISO callbacks publish terminal state before waking the worker; idle
and pending-I/O workers have no periodic polling timer. Only a failed abort or
release API uses a named 10 ms retry interval, retaining native storage. Direct
standalone diagnostics retain their separately named polling intervals.

Named constants replace protocol offsets, request/descriptor codes, endpoint
masks, Linux wire statuses, resource bounds and diagnostic parameters. Existing
Apple binding constants replace raw IOReturn and connection-speed values. Raw
regression vectors remain literal to verify wire values independently.

Eight added regressions cover actual scratch-budget rejection, per-pipe Busy
head blocking/cancellation, sequence wraparound, idle sleeping and callback wake,
failed-abort retry, a queued topology barrier, latched pre-wait notification, and
64 immediate completions without inter-request waits. The last fixture measures
wait calls directly (at most the final idle wait), rather than enforcing a fragile
wall-clock threshold. One debug run retired the 64-request fake-driver burst in
approximately **0.52 ms**; this is not a physical USB latency benchmark.

Validation: **94 macOS workspace tests and 89 Linux workspace tests passed**, each
with zero failures and one intentional ignored corpus-generation test. The five
Python recorder/orchestration checks also passed. Linux used Apple's `container`
CLI. Logs are `m8-review-macos-tests.log` and `m8-review-linux-tests.log` in the
private lab root. Hardware capture was not rerun for this software review; the
hardware observations above continue to identify their original archived binary.

The optimized exporter build, workspace Clippy with warnings denied, formatting
and whitespace checks passed. A final Linux workspace/all-target compile with
`RUSTFLAGS=-Dwarnings` passed after gating the diagnostic-only imports to macOS;
its log is `m8-review-linux-check.log`. No Apple `container` compatibility, setup
or execution issue was encountered. No new hardware trial or privileged service
was started for this review.
