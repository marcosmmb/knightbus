"""Recorder fault tests; no USB open/capture and no sudo required."""
import importlib.util
import io
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch, MagicMock

SCRIPT = Path(__file__).with_name("crash_lab.py")
SPEC = importlib.util.spec_from_file_location("lab", SCRIPT)
LAB = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LAB)


class SinkTests(unittest.TestCase):
    def test_kernel_recorder_requires_mirrored_banner_before_readiness(self):
        for output, mirror_failure, success in [
            (b'Filtering the log data using "test"\n', False, True),
            (b'log: permission denied\n', False, False),
            (b'Filtering the log data using "test"\n', True, False),
        ]:
            with self.subTest(output=output, mirror_failure=mirror_failure), tempfile.TemporaryDirectory() as temp:
                process = MagicMock()
                process.stdout = io.BytesIO(output)
                process.poll.return_value = 0
                mirror = MagicMock()
                if mirror_failure:
                    mirror.event.side_effect = RuntimeError("missing remote ACK")
                startup = LAB.queue.Queue()
                stop = threading.Event()
                stop.set()
                with patch.object(LAB, "Mirror", return_value=mirror), patch.object(LAB.subprocess, "Popen", return_value=process):
                    LAB.stream_logs(Path(temp), stop, startup)
                if success:
                    LAB.await_kernel_recorder(startup, timeout=0)
                    self.assertEqual((Path(temp) / "kernel-usb-hid.jsonl").read_bytes(), output)
                    mirror.event.assert_called_once()
                else:
                    with self.assertRaisesRegex(RuntimeError, "USB trial refused"):
                        LAB.await_kernel_recorder(startup, timeout=0)
        with self.assertRaisesRegex(RuntimeError, "did not become ready"):
            LAB.await_kernel_recorder(LAB.queue.Queue(), timeout=0)

    def test_authenticated_ack_follows_persisted_record_and_wrong_token_is_refused(self):
        with tempfile.TemporaryDirectory(prefix="m8-sink-") as temp:
            directory = Path(temp)
            token = secrets.token_hex(32)
            (directory / "token").write_text(token)
            proc = subprocess.Popen([sys.executable, str(SCRIPT), "sink", temp], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                deadline = time.monotonic() + 5
                while not (directory / "sink.json").exists():
                    self.assertLess(time.monotonic(), deadline)
                    time.sleep(0.01)
                port = json.loads((directory / "sink.json").read_text())["port"]
                with socket.create_connection(("127.0.0.1", port), timeout=2) as conn:
                    stream = conn.makefile("rwb", buffering=0)
                    stream.write(b"incorrect\n")
                    self.assertEqual(stream.readline(), b"")
                    stream.close()
                with socket.create_connection(("127.0.0.1", port), timeout=2) as conn:
                    stream = conn.makefile("rwb", buffering=0)
                    stream.write(token.encode() + b"\n")
                    self.assertEqual(stream.readline(), b"READY\n")
                    event = {"seq": 19, "pid": 42, "operation": "test.no-usb"}
                    stream.write((json.dumps(event) + "\n").encode())
                    self.assertEqual(stream.readline(), b"OK 19\n")
                    self.assertEqual(json.loads((directory / "mirror.jsonl").read_text()), event)
                    stream.close()
            finally:
                proc.terminate()
                proc.wait(timeout=5)

    @unittest.skipUnless(sys.platform == "darwin" and os.environ.get("KNIGHTBUS_M8_TEST_BINARY"), "requires built macOS diagnostic binary")
    def test_native_journal_requires_remote_ack_and_retains_local_record_on_failure(self):
        for acknowledge in [True, False]:
            with self.subTest(acknowledge=acknowledge), tempfile.TemporaryDirectory(prefix="m8-j-") as temp:
                directory = Path(temp)
                # /var/folders temporary paths can exceed sockaddr_un's limit.
                with tempfile.TemporaryDirectory(prefix="m8-s-", dir="/private/tmp") as socket_dir:
                    sock_path = Path(socket_dir) / "s"
                    listener = socket.socket(socket.AF_UNIX)
                    listener.bind(str(sock_path))
                    listener.listen(1)
                    listener.settimeout(5)
                    token = secrets.token_hex(32)
                    (directory / "token").write_text(token)
                    (directory / "target").write_text("02120000 046d c215")
                    (directory / "socket-path").write_text(str(sock_path))
                    received = []
                    errors = []
                    def serve():
                        try:
                            with listener.accept()[0] as conn:
                                stream = conn.makefile("rwb", buffering=0)
                                if stream.readline().strip() != token.encode():
                                    raise AssertionError("wrong token")
                                stream.write(b"READY\n")
                                while True:
                                    line = stream.readline()
                                    if not line:
                                        break
                                    item = json.loads(line)
                                    received.append(item)
                                    # Verify local persistence is ordered before
                                    # the record is delivered for a remote ACK.
                                    logs = list(directory.glob("native-*.jsonl"))
                                    if not logs or line not in logs[0].read_bytes():
                                        raise AssertionError("remote record precedes local write")
                                    if not acknowledge:
                                        stream.write(b"NO\n")
                                        break
                                    stream.write(f"OK {item['seq']}\n".encode())
                                stream.close()
                        except Exception as error:
                            errors.append(error)
                    worker = threading.Thread(target=serve)
                    worker.start()
                    try:
                        result = subprocess.run([os.environ["KNIGHTBUS_M8_TEST_BINARY"], "journal-check", "--lab-journal", temp], capture_output=True, timeout=10)
                        worker.join(timeout=5)
                        self.assertFalse(worker.is_alive())
                        self.assertEqual(errors, [])
                        self.assertEqual(result.returncode == 0, acknowledge, result.stderr)
                        self.assertEqual(received[0]["event"], "journal.ready")
                        self.assertEqual(len(received), 3 if acknowledge else 1)
                        if not acknowledge:
                            self.assertIn(b"missing durable remote ACK", result.stderr)
                    finally:
                        listener.close()


if __name__ == "__main__":
    unittest.main()
