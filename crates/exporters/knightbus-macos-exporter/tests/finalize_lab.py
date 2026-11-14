#!/usr/bin/env python3
"""One bounded M8 hosting/lifecycle trial. Invoked by the durable crash recorder.

No persistent installation, automatic restart, arbitrary command mailbox, or
sudo policy change. Every subprocess is the archived fixture or an OS utility.
"""
import json
import os
from pathlib import Path
import plistlib
import queue
import shutil
import tempfile
import subprocess
import sys
import threading
import time

from crash_lab import Mirror, command, digest, save


def wait_for(check, timeout, description):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.05)
    raise RuntimeError('timeout: ' + description)


def finalize(directory):
    directory = Path(directory).resolve()
    binary = directory / 'knightbus-macos-exporter'
    manifest = json.loads((directory / 'manifest.json').read_text())
    mirror = Mirror(directory)
    child = None
    label = 'com.knightbus.m8.' + directory.name
    service = 'system/' + label
    submitted = False
    staging = None
    results = {}

    def record(name, value):
        mirror.event(name, value)
        results[name] = value
        save(directory / 'finalization.json', json.dumps(results, indent=2))

    def execute(name, args, timeout=25, expected=0):
        mirror.event(name + '.begin', {'argv': args})
        result = command(args, timeout=timeout)
        save(directory / (name + '.json'), json.dumps(result, indent=2))
        print(result['stdout'], end='', flush=True)
        record(name + '.end', result)
        if result['returncode'] != expected:
            raise RuntimeError(f'{name} failed: {result}')
        return result

    def fixture(*args, recorded=True):
        return [str(binary), *args] + (['--lab-journal', str(directory)] if recorded else [])

    def hid(name):
        execute(name, ['/usr/bin/sudo', '-u', '#' + str(manifest['developer_uid']),
                       manifest['python'], str(directory / 'local_hid.py')])

    def generation():
        result = command([str(binary), 'list'])
        if result['returncode']:
            raise RuntimeError('inventory failed: ' + result['stderr'])
        return next((line.split()[0] for line in result['stdout'].splitlines()
                     if 'location=0x02120000 vid=046d pid=c215' in line), None)

    try:
        if manifest['case'] == 'finalization':
            hid('local-hid-before')
            refused = execute('unprivileged-capture',
                              ['/usr/bin/sudo', '-u', '#' + str(manifest['developer_uid'])]
                              + fixture('qualify', '02120000', '1', '--capture', recorded=False), expected=1)
            if 'error=Denied' not in refused['stderr']:
                raise RuntimeError('unprivileged capture did not report Denied')
            # Prove normal library/CLI use has no remote-journal dependency. The
            # supervisor and kernel recorder still preserve coarse crash evidence.
            execute('normal-embedded', fixture('qualify', '02120000', '1', '--capture', recorded=False))
            hid('local-hid-after-embedded')
    
        # Root launchd jobs have a different privacy context from Terminal.
        # Stage only this fixed trial outside Documents, with root-only access.
        staging = Path(tempfile.mkdtemp(prefix='knightbus-m8-service-', dir='/private/var/tmp'))
        for name in ['knightbus-macos-exporter', 'target', 'token', 'socket-path']:
            shutil.copy2(directory / name, staging / name)
        if digest(staging / 'knightbus-macos-exporter') != manifest['binary_sha256']:
            raise RuntimeError('staged service binary identity mismatch')
        args = [str(staging / 'knightbus-macos-exporter'), 'qualify', '02120000', '1',
                '--capture', '--lab-journal', str(staging)]
        plist = staging / 'temporary-launchd.plist'
        save(plist, plistlib.dumps({'Label': label, 'ProgramArguments': args,
                                   'RunAtLoad': True, 'KeepAlive': False,
                                   'StandardOutPath': str(staging / 'launchd.log'),
                                   'StandardErrorPath': str(staging / 'launchd-error.log')}))
        os.chmod(plist, 0o600)
        record('launchd.bootstrap.begin', {'service': service, 'argv': args, 'staging': str(staging)})
        submitted = True  # also attempt cleanup if bootstrap partially fails
        execute('launchd-bootstrap', ['/bin/launchctl', 'bootstrap', 'system', str(plist)])

        def exited():
            state = command(['/bin/launchctl', 'print', service])
            save(directory / 'launchd-context.json', json.dumps(state, indent=2))
            if state['returncode']:
                raise RuntimeError('launchd job disappeared')
            text = state['stdout']
            if 'state = not running' in text and 'last exit code = ' in text:
                if 'last exit code = 0\n' not in text:
                    raise RuntimeError('launchd child failed: ' + text)
                return True
            return False

        wait_for(exited, 30, 'launchd child exit')
        output = (staging / 'launchd.log').read_text()
        if 'shutdown=PASS credits=0' not in output:
            raise RuntimeError('launchd engine did not finish')
        record('launchd.engine', {'result': 'PASS', 'stdout': output})
        execute('launchd-bootout', ['/bin/launchctl', 'bootout', service])
        submitted = False
        hid('local-hid-after-launchd')

        before = generation()
        if not before:
            raise RuntimeError('joystick absent before owner-exit trial')
        args = fixture('hold', '02120000', '--capture')
        mirror.event('owner.start', {'argv': args, 'generation': before})
        with open(directory / 'owner.log', 'wb', buffering=0) as output:
            child = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            ready = queue.Queue()

            def copy():
                for line in child.stdout:
                    output.write(line)
                    if b'crash_probe=READY' in line:
                        ready.put(True)
                ready.put(False)

            reader = threading.Thread(target=copy, daemon=True)
            reader.start()
            try:
                is_ready = ready.get(timeout=15)
            except queue.Empty as error:
                raise RuntimeError('owner never reached captured transfer stage') from error
            if not is_ready or child.poll() is not None:
                raise RuntimeError('owner exited before kill fixture')
            mirror.event('owner.kill', {'pid': child.pid, 'signal': 'SIGKILL'})
            child.kill()
            code = child.wait(timeout=10)
            reader.join(timeout=5)
            child.stdout.close()
            child = None
            if code != -9:
                raise RuntimeError('owner not killed as expected')
        after = wait_for(lambda: (current if (current := generation()) != before else None),
                         10, 'new generation after owner exit')
        record('owner.exit', {'returncode': code, 'before': before, 'after': after})
        execute('owner-reclaim', fixture('qualify', '02120000', '1', '--capture'))
        hid('local-hid-after-owner-reclaim')
        record('automated-checks', 'PASS; starting idle physical removal')
        # Stream readiness so the sole physical action happens at the right time.
        args = fixture('idle-unplug', '02120000', '--capture')
        mirror.event('idle-unplug.begin', {'argv': args})
        child = subprocess.Popen(args)
        code = child.wait(timeout=45)
        child = None
        record('idle-unplug.end', {'returncode': code})
        if code:
            raise RuntimeError('idle physical removal failed')
        record('finalization', 'PASS')
        print('m8_finalization=PASS Reconnect the joystick now.', flush=True)
    finally:
        if child is not None and child.poll() is None:
            child.kill()
            child.wait(timeout=10)
        if submitted:
            cleanup = command(['/bin/launchctl', 'bootout', service])
            save(directory / 'launchd-cleanup.json', json.dumps(cleanup, indent=2))
        if staging is not None:
            for pattern in ['native-*.jsonl', 'launchd*.log', 'temporary-launchd.plist']:
                for source in staging.glob(pattern):
                    save(directory / source.name, source.read_bytes())
            # Preserve staged evidence on any cleanup problem; never delete
            # files still potentially in use by a loaded service.
            absent = command(['/bin/launchctl', 'print', service])['returncode'] != 0
            if absent:
                shutil.rmtree(staging)
            else:
                save(directory / 'staging-retained.txt', str(staging))
        mirror.close()


if __name__ == '__main__':
    finalize(sys.argv[1])
