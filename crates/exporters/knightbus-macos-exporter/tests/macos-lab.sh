#!/bin/bash
# Development-only test of the explicitly selected Logitech 046d:c215.
# No network listener, driver, entitlement, signing policy, storage-device writes,
# or persistent service installation. Retains diagnostic logs in /private/tmp.
set -eu
echo 'DISABLED: this batch triggered a macOS kernel panic. See docs/milestone8-validation.md. Do not rerun on the development Mac.' >&2
exit 2
if [ "$(id -u)" -ne 0 ]; then echo 'Run this reviewed lab script with sudo.' >&2; exit 1; fi
if [ "$#" -ne 2 ]; then echo 'Usage: macos-lab.sh ABSOLUTE_BINARY HEX_LOCATION' >&2; exit 1; fi
binary="$1"
location="$2"
lab_dir=$(mktemp -d /private/tmp/knightbus-m8-lab.XXXXXX)
label="com.knightbus.m8.lab.$$"
child=''
submitted=0
cleanup() {
    if [ -n "$child" ]; then kill "$child" 2>/dev/null || true; wait "$child" 2>/dev/null || true; fi
    if [ "$submitted" -eq 1 ]; then launchctl remove "$label" 2>/dev/null || true; fi
    chmod -R a+rX "$lab_dir"
}
trap cleanup EXIT
echo "Evidence: $lab_dir"
sw_vers > "$lab_dir/environment.txt"
csrutil status >> "$lab_dir/environment.txt"
shasum -a 256 "$binary" >> "$lab_dir/environment.txt"
"$binary" access "$location" > "$lab_dir/ordinary-root.log" 2>&1
"$binary" qualify "$location" 10 --capture > "$lab_dir/engine.log" 2>&1
"$binary" reset "$location" --capture > "$lab_dir/reset.log" 2>&1
"$binary" hold "$location" --capture > "$lab_dir/crash.log" 2>&1 &
child=$!
for n in {1..50}; do
    if /usr/bin/grep -q 'crash_probe=READY' "$lab_dir/crash.log"; then break; fi
    sleep 0.1
done
/usr/bin/grep -q 'crash_probe=READY' "$lab_dir/crash.log"
kill -KILL "$child"
wait "$child" 2>/dev/null || true
child=''
sleep 1
"$binary" qualify "$location" 1 --capture > "$lab_dir/after-crash.log" 2>&1
# The unique temporary system-domain job is removed on exit.
launchctl submit -l "$label" -o "$lab_dir/launchd.log" -e "$lab_dir/launchd-error.log" -- "$binary" qualify "$location" 1 --capture
submitted=1
launchctl print "system/$label" > "$lab_dir/launchd-context.txt"
for n in {1..150}; do
    if /usr/bin/grep -q 'shutdown=PASS credits=0' "$lab_dir/launchd.log" 2>/dev/null; then break; fi
    sleep 0.1
done
/usr/bin/grep -q 'shutdown=PASS credits=0' "$lab_dir/launchd.log"
launchctl print "system/$label" > "$lab_dir/launchd-final.txt"
launchctl remove "$label"
submitted=0
"$binary" list > "$lab_dir/after-list.txt"
echo 'Repeated engine cycles, reset, creator crash/reclaim and launchd tests: PASS'
