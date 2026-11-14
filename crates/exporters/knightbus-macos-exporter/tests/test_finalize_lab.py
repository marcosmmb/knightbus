"""Host orchestration failure cleanup, without launchctl, sudo or USB access."""
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import MagicMock, patch

sys.path.insert(0, str(Path(__file__).parent))
SPEC = importlib.util.spec_from_file_location('finalize', Path(__file__).with_name('finalize_lab.py'))
LAB = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LAB)


class FinalizeTests(unittest.TestCase):
    def exercise(self, fail_bootstrap):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            for name in ['knightbus-macos-exporter', 'target', 'token', 'socket-path']:
                (directory / name).write_text('fixture')
            (directory / 'manifest.json').write_text(json.dumps({'developer_uid': 501, 'python': sys.executable, 'case': 'finalization', 'binary_sha256': LAB.digest(directory / 'knightbus-macos-exporter')}))
            (directory / 'launchd.log').write_text('cycle=1 shutdown=PASS credits=0\n')
            calls = []
            inventory = iter(['old', 'new'])

            def command(args, timeout=30):
                calls.append(args)
                out, err, code = '', '', 0
                if args[-1] == 'list':
                    out = f'registry={next(inventory)} location=0x02120000 vid=046d pid=c215\n'
                if args[0] == '/usr/bin/sudo' and 'qualify' in args:
                    code, err = 1, 'error=Denied'
                if args[:2] == ['/bin/launchctl', 'bootstrap']:
                    (Path(args[-1]).parent / 'launchd.log').write_text('cycle=1 shutdown=PASS credits=0\n')
                    if fail_bootstrap:
                        code = 5
                if args[:2] == ['/bin/launchctl', 'print']:
                    out = 'state = not running\nlast exit code = 0\n'
                    if any(c[:2] == ['/bin/launchctl', 'bootout'] for c in calls):
                        code = 3
                return {'argv': args, 'stdout': out, 'stderr': err, 'returncode': code}

            owner = MagicMock()
            owner.stdout = io.BytesIO(b'crash_probe=READY\n')
            owner.poll.return_value = None
            owner.wait.return_value = -9
            idle = MagicMock()
            idle.wait.return_value = 0
            with patch.object(LAB, 'command', side_effect=command), \
                 patch.object(LAB, 'Mirror'), \
                 patch.object(LAB.subprocess, 'Popen', side_effect=[owner, idle]) as popen, \
                 patch('builtins.print'):
                if fail_bootstrap:
                    with self.assertRaisesRegex(RuntimeError, 'launchd-bootstrap failed'):
                        LAB.finalize(directory)
                    popen.assert_not_called()
                else:
                    LAB.finalize(directory)
                    owner.kill.assert_called_once()
                    self.assertEqual(json.loads((directory / 'finalization.json').read_text())['finalization'], 'PASS')
                cleanup = [c for c in calls if c[:2] == ['/bin/launchctl', 'bootout']]
                self.assertEqual(cleanup, [['/bin/launchctl', 'bootout', 'system/com.knightbus.m8.' + directory.name]])

    def test_complete_trial_removes_temporary_service_and_kills_only_owned_child(self):
        self.exercise(False)

    def test_partial_bootstrap_failure_still_removes_its_exact_service(self):
        self.exercise(True)


if __name__ == '__main__':
    unittest.main()
