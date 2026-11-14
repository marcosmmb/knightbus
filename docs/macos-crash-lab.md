# macOS crash investigation and recovery

The owner explicitly authorized continued M8 hardware investigation after the
2026-11-13 panic, including further crashes, with durable evidence as a prerequisite.
This authorizes diagnostic capture; it does not establish product support or
resolve the [original panic](milestone8-validation.md). No OS-security change,
third-party driver or persistent automatic USB test is required.

Normal explicit development capture is available without this recorder after the
owner deferred further panic investigation. A recorded diagnostic process can capture only the
recorded Logitech `046d:c215` at `0x02120000`, after `--lab-journal` successfully
opens both local storage and an authenticated remote recorder. The old batch
script remains disabled. New experiments run individually from immutable-by-hash
prepared artifacts, with a fresh run directory for every trial.

## Evidence before the first USB operation

`tests/crash_lab.py prepare` creates a private directory under
`/Users/marcos/Documents/KnightBusLab/`, never `/tmp`. It records:

- The exact executable, SHA-256 and Mach-O UUID, a complete dSYM produced before
  subsequent builds can replace debug-map objects, and a source archive including
  the uncommitted new backend. The base Git revision and working-tree diff are retained.
- The matching installed kernel symbol image, its UUID and hash; OS/build, boot
  time, SIP status, USB/HID topology and loaded kernel extensions.
- The exact test case/arguments, selected physical port and remote evidence path.
  No USB payloads or serial-string descriptors are logged. Local registry snapshots
  filter serial/signature properties. Kernel/panic logs can contain system information
  and stay in the private lab directory, outside the repository.

Those artifacts are copied to Linux at `/var/tmp/knightbus-m8/RUN_ID/` over SSH;
executable, source, dSYM, script and kernel archive hashes are checked remotely.
The recorder binds only Linux loopback. An SSH Unix-socket forward and a random
per-run token authenticate the record stream. It is not an unauthenticated LAN service.

Each native operation's BEGIN record contains a local wall/monotonic timestamp,
process/thread, operation/cycle and physical generation. Rust writes and `fsync`s
the local record, uses Apple's `F_FULLFSYNC`, then waits for the Linux sink to
`fsync` its copy and acknowledge that sequence. Only then does the operation start.
Return/callback and teardown records retain results and IOReturn codes. New work
is refused after recording failure; necessary cleanup still runs with best-effort
records. Kernel USB/HID log streaming is separately saved and mirrored, with a
32 MiB local limit and explicit truncation/error markers. New prepared trials
also require the kernel stream's startup banner to be persisted locally and
remotely before starting the USB child; a missing banner or failed recorder
refuses the trial. This additional check postdates the first two successful replays.

This substantially improves crash evidence but cannot guarantee preservation under
every storage or network failure. Recording alters timing; a passing instrumented
run is not proof that the original race is fixed. `F_FULLFSYNC` is intentionally
limited to this opt-in lab path, consistent with Apple's
[persistence guidance](https://developer.apple.com/documentation/xcode/reducing-disk-writes).

## Preparation and one supervised trial

Use a fresh target directory for this investigation. The initial default-target
`cargo check` reused stale core metadata despite updated source; rebuilding under
`target/m8-instrumented` resolved that build-cache inconsistency.

```sh
cargo build -p knightbus-macos-exporter --target-dir target/m8-instrumented
PYTHONDONTWRITEBYTECODE=1 KNIGHTBUS_M8_TEST_BINARY="$PWD/target/m8-instrumented/debug/knightbus-macos-exporter" python3 crates/exporters/knightbus-macos-exporter/tests/test_crash_lab.py -v
python3 crates/exporters/knightbus-macos-exporter/tests/crash_lab.py prepare \
  --repo "$PWD" --binary "$PWD/target/m8-instrumented/debug/knightbus-macos-exporter" \
  --evidence /Users/marcos/Documents/KnightBusLab \
  --remote root@192.168.88.242 --case capture-worker
```

Preparation performs a no-USB recorder self-test. It prints the exact `sudo`
command for the archived script/run. Run that command in Terminal when root
access is needed. It verifies hashes and creates `STARTED` exclusively, preventing
accidental reuse of a crashed or completed run directory. Root access is not
stored or broadened in sudoers, and no password is sent through chat.

The initial `capture-worker` case performs exactly one capture/claim/release on
the backend worker. It does not send endpoint transfers, change configuration,
reset the device explicitly, or install a daemon. Descriptor reads are still
required for inventory and claim validation. `engine-one` is a separate single
engine cycle including the existing standard control/configuration checks.
`engine-three` repeats that cycle three times to investigate recapture after
release/rematching. Every cycle and native operation is journaled; the run stops
on the first error. It is a separate prepared trial after reviewing the single-cycle
evidence. Do not combine later cases into an unobserved long batch.
`engine-ten` repeats ten engine cycles, matching the cycle count of the original
failed batch while retaining the new per-operation instrumentation. It excludes
the old batch's reset, process-kill and daemon stages. A passing result still
does not resolve the effect of instrumentation on the original panic's timing.
`original-prefix` adds the old batch's ordinary root interface-claim diagnostic
immediately before those ten engine cycles. This tests the effect of that
preceding open/claim/release sequence; it still excludes later reset, process-kill
and daemon stages and records each operation.

`native-faults` checks a reserved-descriptor request returning STALL, a subsequent
valid descriptor read, CLEAR_FEATURE(ENDPOINT_HALT) on the healthy `0x81` pipe,
interrupt input, native abort and explicit reset. It allows up to eight abort
attempts: a normal completion may legitimately win the cancellation race. Only
an actual canceled completion marks native abort PASS; if none occurs, the trial
prints native abort UNPROVEN and continues to reset. Exit zero alone does not
qualify an unobserved canceled completion.
Clearing a healthy pipe does not qualify recovery from a physically stalled data
endpoint. Each error stops the fixture and drains owned requests.

`hot-unplug` attaches through the shared engine, verifies a real interrupt report,
then prints `unplug=READY`. Disconnect only the selected joystick at that point
and leave it disconnected until completion (35-second removal deadline). The
fixture keeps interrupt traffic active, requires the engine's own removal
notification to close the session with `Unavailable` and remove inventory, then
checks zero retained credits. Reconnect afterward; reconnection does not
automatically reauthorize or capture the new generation.

The supervisor flushes stdout, records before/after context and starts bounded
kernel logging. A 60-second child timeout collects a one-second userspace sample,
then terminates only that child if it cannot finish. A kernel panic naturally
interrupts all local processes; the remote recorder survives independently for
up to six hours. No hardware test resumes automatically after boot.

## After a reboot

Read `RECOVERY.txt` in the prepared directory. The collector performs no USB
opens or captures:

```sh
python3 /Users/marcos/Documents/KnightBusLab/RUN_ID/crash_lab.py collect \
  /Users/marcos/Documents/KnightBusLab/RUN_ID
```

It preserves new panic/reset/process reports, hashes them, records the new boot
context, retrieves the Linux journal and exports retained USB/HID unified logs.
The current repository collector also archives the recovered reports and local
log tails back to Linux with SHA-256 verification, retaining the original live
mirror unchanged. Older archived collectors lack this additional copy; run the
current repository's `crash_lab.py collect RUN_DIRECTORY` when needed. The
collector's script hash is saved alongside the recovery evidence. This can
preserve a final locally flushed callback interrupted before its remote ACK;
it does not retroactively mark that callback as live-acknowledged.
If the SSH agent was cleared, unlock the existing key in Terminal with
`ssh-add --apple-use-keychain ~/.ssh/id_ed25519` and rerun collection. Permission,
network and retention failures are recorded instead of silently omitting evidence.
Root-only unified-log archives or further `sysdiagnose` collection can be added
to the same run if the ordinary collector reports insufficient access.

Find the final BEGIN lacking an END in both journals; correlate its cycle,
registry ID and native API with the panic's thread stacks. Use the archived
executable/dSYM and kernel UUID, not the latest build, for symbolication.
Preserve failed runs unchanged before testing any proposed fix.


## Final M8 development checks

`--case finalization` archives and verifies `finalize_lab.py` and `local_hid.py`
alongside the executable, symbols and source. One root invocation checks ordinary
unprivileged capture refusal; a normal capture cycle without native per-operation
journaling (supervisor/kernel evidence still active); a temporary system launchd
job; exporter SIGKILL and fresh-generation reclaim; and idle unplug. Only the
selected Logitech device is exercised. The runner removes its exact temporary
service on success or failure; it installs no persistent helper or sudo rule.
A reboot drops the temporary launchd registration and does not replay this test.
The total deadline is 210 seconds and every subprocess has a shorter bound.

The final stage prints `unplug=READY`: disconnect the joystick within 35 seconds
and leave it disconnected until completion, then reconnect. Local application
recovery is measured automatically before that stage with an ordinary-user IOHID
consumer. Per-stage results are in `finalization.json`, `launchd*.json/log` and
`local-hid*.json`; `collect` backs these up to Linux with the native/kernel logs.
The owner deferred further panic investigation; this recorder is optional for
normal explicitly authorized backend use, mandatory once a recorded trial starts.


The `finalization-remaining` case resumes after independently recorded normal
capture/refusal checks; it still creates a new immutable trial directory and does
not replay an old `STARTED` directory. Service binaries and recorder state are
staged root-only under `/private/var/tmp`, and the authenticated SSH socket is
created in a private `/private/tmp/knightbus-m8-*` directory. Background launchd
jobs must not depend on Terminal's access to Documents. Native service journals
and logs are copied back durably after bootout and included in recovery archives.
The staged directory is removed only after launchctl confirms the job is absent.
The archival evidence itself never depends on those temporary directories.
