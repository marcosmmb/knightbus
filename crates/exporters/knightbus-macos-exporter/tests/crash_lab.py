#!/usr/bin/env python3
"""Durable, mirrored M8 crash evidence. No automatic hardware replay after reboot.

prepare runs as the developer; run executes one archived trial as root; collect
is read-only with respect to USB and can be rerun after reboot. sink runs on the
Linux lab over an authenticated SSH Unix-socket forward, never a public listener.
"""
import argparse
import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import queue
import secrets
import shlex
import shutil
import socket
import socketserver
import subprocess
import sys
import tarfile
import threading
import tempfile
import time

SSH = ["ssh", "-o", "BatchMode=yes", "-o", "ForwardAgent=no", "-o", "ConnectTimeout=8"]
CASES = {"capture-worker": ["capture-worker", "02120000", "1", "--capture"],
         "engine-one": ["qualify", "02120000", "1", "--capture"],
         "engine-three": ["qualify", "02120000", "3", "--capture"],
         "engine-ten": ["qualify", "02120000", "10", "--capture"],
         "original-prefix": ["access-then-qualify", "02120000", "10", "--capture"],
         "native-faults": ["faults", "02120000", "--capture"],
         "hot-unplug": ["unplug", "02120000", "--capture"],
         "finalization": [],
         "finalization-remaining": []}


def flush(file):
    file.flush()
    os.fsync(file.fileno())
    if sys.platform == "darwin":
        fcntl.fcntl(file.fileno(), 51)  # F_FULLFSYNC, not merely stdio flush.


def save(path, data):
    with open(path, "wb") as file:
        file.write(data.encode() if isinstance(data, str) else data)
        flush(file)
    parent = os.open(str(Path(path).parent), os.O_RDONLY)
    try:
        os.fsync(parent)
    finally:
        os.close(parent)


def digest(path):
    with open(path, "rb") as file:
        h = hashlib.sha256()
        for block in iter(lambda: file.read(1024 * 1024), b""):
            h.update(block)
        return h.hexdigest()


def command(args, timeout=30):
    r = subprocess.run(args, capture_output=True, timeout=timeout)
    return {"argv": args, "returncode": r.returncode,
            "stdout": r.stdout.decode(errors="replace"), "stderr": r.stderr.decode(errors="replace")}


def remote(host, args):
    return subprocess.check_output(SSH + [host, shlex.join(args)], text=True, timeout=60)


def snapshot(directory, label):
    directory = Path(directory)
    queries = [["sw_vers"], ["sysctl", "kern.boottime", "hw.model", "hw.memsize"],
               ["csrutil", "status"], ["ioreg", "-p", "IOUSB", "-w0"],
               ["ioreg", "-r", "-n", "Extreme 3D pro", "-w0"],
               ["ps", "-axo", "pid,ppid,user,comm"], ["kmutil", "showloaded"]]
    result = []
    for args in queries:
        try:
            item = command(args)
        except subprocess.TimeoutExpired:
            item = {"argv": args, "timeout": True}
        if args[0] == "ioreg" and "stdout" in item:
            item["stdout"] = "\n".join(line for line in item["stdout"].splitlines()
                                       if "serial" not in line.lower() and "UsbDeviceSignature" not in line)
        result.append(item)
    save(directory / f"{label}.json", json.dumps(result, indent=2))


class Mirror:
    def __init__(self, directory):
        self.directory = Path(directory)
        self.seq = 0
        self.file = open(self.directory / f"supervisor-{os.getpid()}-{threading.get_ident()}.jsonl", "ab", buffering=0)
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(8)
        self.sock.connect((self.directory / "socket-path").read_text().strip())
        self.stream = self.sock.makefile("rwb", buffering=0)
        self.stream.write((self.directory / "token").read_bytes().strip() + b"\n")
        if self.stream.readline(100) != b"READY\n":
            raise RuntimeError("remote recorder authentication failed")

    def event(self, operation, detail):
        self.seq += 1
        record = {"pid": os.getpid(), "channel": threading.get_ident(), "seq": self.seq, "wall_ns": time.time_ns(),
                  "event": "supervisor", "operation": operation, "detail": detail}
        line = (json.dumps(record) + "\n").encode()
        self.file.write(line)
        flush(self.file)
        self.stream.write(line)
        if self.stream.readline(100) != f"OK {self.seq}\n".encode():
            raise RuntimeError("remote journal did not acknowledge durability")

    def close(self):
        self.stream.close()
        self.sock.close()
        self.file.close()


def sink(directory):
    directory = Path(directory)
    token = (directory / "token").read_bytes().strip()
    lock = threading.Lock()
    output = open(directory / "mirror.jsonl", "ab", buffering=0)

    class Handler(socketserver.StreamRequestHandler):
        def handle(self):
            self.request.settimeout(300)
            if not secrets.compare_digest(self.rfile.readline(128).strip(), token):
                return
            self.wfile.write(b"READY\n")
            while True:
                line = self.rfile.readline(32769)
                if not line:
                    return
                if len(line) > 32768 or not line.endswith(b"\n"):
                    return
                item = json.loads(line)
                if not isinstance(item.get("seq"), int) or not isinstance(item.get("pid"), int):
                    return
                with lock:
                    output.write(line)
                    flush(output)
                self.wfile.write(f"OK {item['seq']}\n".encode())

    class Server(socketserver.ThreadingTCPServer):
        daemon_threads = True
        allow_reuse_address = False
        permits = threading.BoundedSemaphore(8)

        def process_request(self, request, address):
            if not self.permits.acquire(blocking=False):
                self.shutdown_request(request)
                return
            super().process_request(request, address)

        def process_request_thread(self, request, address):
            try:
                super().process_request_thread(request, address)
            finally:
                self.permits.release()

    with Server(("127.0.0.1", 0), Handler) as server:
        save(directory / "sink.json", json.dumps({"port": server.server_address[1], "pid": os.getpid()}))
        # Survives a Mac panic/SSH loss. Bounded lifetime; no remote installation.
        threading.Timer(6 * 3600, server.shutdown).start()
        server.serve_forever()


def start_sink(directory):
    directory = Path(directory)
    with open(directory / "sink.log", "ab") as log:
        subprocess.Popen([sys.executable, __file__, "sink", str(directory)],
                         stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
    for _ in range(100):
        if (directory / "sink.json").exists():
            print((directory / "sink.json").read_text())
            return
        time.sleep(0.1)
    raise RuntimeError("remote sink did not start")


def prepare(args):
    repo = Path(args.repo).resolve()
    root = Path(args.evidence).resolve()
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    run_id = datetime.datetime.now().strftime("m8-%m%d-%H%M%S-") + secrets.token_hex(2)
    directory = root / run_id
    directory.mkdir(mode=0o700)
    # launchd cannot rely on access to a developer Documents directory. Only
    # this ephemeral authenticated socket lives outside durable evidence storage.
    socket_directory = Path(tempfile.mkdtemp(prefix="knightbus-m8-", dir="/private/tmp"))
    socket_path = socket_directory / "mirror.sock"
    if len(str(socket_path).encode()) >= 104:
        raise RuntimeError("evidence path too long for a macOS Unix socket")
    save(directory / "token", secrets.token_hex(32))
    os.chmod(directory / "token", 0o600)
    save(directory / "socket-path", str(socket_path))
    save(directory / "target", "02120000 046d c215\n")
    binary = directory / "knightbus-macos-exporter"
    shutil.copy2(Path(args.binary).resolve(), binary)
    shutil.copy2(__file__, directory / "crash_lab.py")
    helpers = {}
    for name in ["finalize_lab.py", "local_hid.py"]:
        shutil.copy2(Path(__file__).with_name(name), directory / name)
        helpers[name] = digest(directory / name)
    # Generate symbols before any later Cargo build changes debug-map objects.
    symbols = command(["xcrun", "dsymutil", str(binary), "-o", str(directory / "exporter.dSYM")], timeout=120)
    save(directory / "dsymutil.json", json.dumps(symbols, indent=2))
    if symbols["returncode"] or "could not find" in symbols["stderr"].lower():
        raise RuntimeError("cannot preserve complete symbols; see dsymutil.json")
    files = subprocess.check_output(["git", "ls-files", "-co", "--exclude-standard", "-z"], cwd=repo).split(b"\0")
    with tarfile.open(directory / "source.tar.gz", "w:gz") as archive:
        for name in sorted(set(os.fsdecode(f) for f in files if f)):
            path = repo / name
            if path.is_file() and "__pycache__" not in path.parts and not name.endswith(".pyc") and (name.startswith(("crates/", "docs/", ".github/")) or name in ["Cargo.toml", "Cargo.lock", "README.md", "LICENSE", "rust-toolchain.toml", ".gitignore", ".editorconfig"]):
                archive.add(path, arcname=name)
    with tarfile.open(directory / "symbols.tar.gz", "w:gz") as archive:
        archive.add(directory / "exporter.dSYM", arcname="exporter.dSYM")
    save(directory / "working-tree.diff", subprocess.check_output(["git", "diff", "--binary"], cwd=repo))
    kernel = Path("/System/Library/Kernels/kernel.release.t8132")
    with tarfile.open(directory / "kernel.tar.gz", "w:gz") as archive:
        archive.add(kernel, arcname=kernel.name)
    manifest = {"run": run_id, "prepared_ns": time.time_ns(), "case": args.case,
                "binary_sha256": digest(binary), "source_sha256": digest(directory / "source.tar.gz"),
                "symbols_sha256": digest(directory / "symbols.tar.gz"),
                "script_sha256": digest(directory / "crash_lab.py"),
                "kernel_sha256": digest(directory / "kernel.tar.gz"),
                "kernel_uuid": command(["xcrun", "dwarfdump", "--uuid", str(kernel)]),
                "helpers": helpers, "developer_uid": os.getuid(), "python": sys.executable,
                "git_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo).decode().strip(),
                "binary_uuid": command(["xcrun", "dwarfdump", "--uuid", str(binary)]),
                "symbols_uuid": command(["xcrun", "dwarfdump", "--uuid", str(directory / "exporter.dSYM")]),
                "remote": args.remote, "remote_directory": f"/var/tmp/knightbus-m8/{run_id}"}
    save(directory / "manifest.json", json.dumps(manifest, indent=2))
    snapshot(directory, "prepared-environment")
    save(directory / "RECOVERY.txt", f"No test restarts automatically. After reboot run:\n{sys.executable} {directory}/crash_lab.py collect {directory}\nRemote copy: {args.remote}:{manifest['remote_directory']}\n")
    remote(args.remote, ["mkdir", "-p", "-m", "700", manifest["remote_directory"]])
    artifacts = ["token", "manifest.json", "crash_lab.py", "source.tar.gz", "symbols.tar.gz",
                 "knightbus-macos-exporter", "working-tree.diff", "prepared-environment.json", "RECOVERY.txt", "dsymutil.json", "kernel.tar.gz", *helpers]
    # Force every artifact to local media before publishing the READY instruction.
    for name in artifacts:
        with open(directory / name, "rb") as file:
            os.fsync(file.fileno())
            fcntl.fcntl(file.fileno(), 51)
    subprocess.run(["scp", "-q"] + [str(directory / name) for name in artifacts]
                   + [f"{args.remote}:{manifest['remote_directory']}/"], check=True)
    for name, key in [("knightbus-macos-exporter", "binary_sha256"), ("source.tar.gz", "source_sha256"), ("symbols.tar.gz", "symbols_sha256"), ("crash_lab.py", "script_sha256"), ("kernel.tar.gz", "kernel_sha256")]:
        actual = remote(args.remote, ["sha256sum", manifest["remote_directory"] + "/" + name])
        if actual.split()[0] != manifest[key]:
            raise RuntimeError("remote artifact digest mismatch: " + name)
    for name, expected in helpers.items():
        if remote(args.remote, ["sha256sum", manifest["remote_directory"] + "/" + name]).split()[0] != expected:
            raise RuntimeError("remote helper digest mismatch: " + name)
    info = json.loads(remote(args.remote, ["python3", manifest["remote_directory"] + "/crash_lab.py", "start-sink", manifest["remote_directory"]]))
    with open(directory / "tunnel.log", "ab") as log:
        tunnel = subprocess.Popen(SSH + ["-N", "-o", "ExitOnForwardFailure=yes", "-o", "ServerAliveInterval=5",
                                        "-o", "ServerAliveCountMax=3", "-L", f"{socket_path}:127.0.0.1:{info['port']}", args.remote],
                                  stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
    save(directory / "recorder-processes.json", json.dumps({"tunnel_pid": tunnel.pid, "sink": info}))
    for _ in range(100):
        if socket_path.exists():
            break
        if tunnel.poll() is not None:
            raise RuntimeError("SSH forward failed; inspect tunnel.log")
        time.sleep(0.1)
    trial = directory / "selftest"
    trial.mkdir(mode=0o700)
    for name in ["target", "token", "socket-path"]:
        shutil.copy2(directory / name, trial / name)
    subprocess.run([str(binary), "journal-check", "--lab-journal", str(trial)], check=True)
    mirror = Mirror(directory)
    mirror.event("prepared", manifest)
    mirror.close()
    save(directory / "READY", "recorder and remote ACK self-test passed; no USB capture performed\n")
    print(directory)
    print(shlex.join(["sudo", sys.executable, str(directory / "crash_lab.py"), "run", str(directory)]))


def stream_logs(directory, stop, startup):
    predicate = 'process == "kernel" AND (eventMessage CONTAINS[c] "USB" OR eventMessage CONTAINS[c] "HID")'
    path = directory / "kernel-usb-hid.jsonl"
    mirror = None
    proc = None
    announced = False
    try:
        with open(path, "ab", buffering=0) as file:
            mirror = Mirror(directory)
            proc = subprocess.Popen(["/usr/bin/log", "stream", "--style", "ndjson", "--level", "debug", "--predicate", predicate],
                                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            def terminate():
                stop.wait()
                proc.terminate()
            threading.Thread(target=terminate, daemon=True).start()
            total = 0
            for line in proc.stdout:
                if total + len(line) > 32 * 1024 * 1024:
                    file.write(b'{"recorder":"size limit reached"}\n')
                    flush(file)
                    proc.terminate()
                    break
                file.write(line)
                total += len(line)
                flush(file)
                if mirror is not None:
                    try:
                        mirror.event("kernel.log", line.decode(errors="replace")[:12000])
                        # Apple's stream prints this banner after accepting its
                        # filter. Require its durable mirror before starting USB.
                        if not announced and line.startswith(b"Filtering the log data using"):
                            startup.put(None)
                            announced = True
                    except (OSError, RuntimeError) as error:
                        file.write((json.dumps({"mirror_error": str(error)}) + "\n").encode())
                        flush(file)
                        mirror.close()
                        mirror = None
            proc.wait()
            if not announced:
                raise RuntimeError("kernel log stream exited before readiness")
    except Exception as error:
        save(directory / "kernel-recorder-error.txt", str(error))
        if not announced:
            startup.put(str(error))
            announced = True
    finally:
        if proc is not None and proc.poll() is None:
            proc.terminate()
            proc.wait(timeout=5)
        if mirror is not None:
            mirror.close()


def await_kernel_recorder(startup, timeout=10):
    try:
        error = startup.get(timeout=timeout)
    except queue.Empty as error:
        raise RuntimeError("kernel log stream did not become ready; USB trial refused") from error
    if error is not None:
        raise RuntimeError("kernel recorder failed; USB trial refused: " + error)


def run(directory):
    if os.getuid() != 0:
        raise RuntimeError("this explicit hardware trial requires sudo")
    directory = Path(directory).resolve()
    manifest = json.loads((directory / "manifest.json").read_text())
    binary = directory / "knightbus-macos-exporter"
    if not (directory / "READY").exists() or digest(binary) != manifest["binary_sha256"] or digest(__file__) != manifest["script_sha256"]:
        raise RuntimeError("prepared trial identity check failed")
    for name, expected in manifest.get("helpers", {}).items():
        if digest(directory / name) != expected:
            raise RuntimeError("prepared helper identity check failed: " + name)
    # Exclusive create makes accidental replay of a completed or crashed run fail.
    try:
        with open(directory / "STARTED", "xb") as file:
            file.write(str(time.time_ns()).encode())
            flush(file)
    except FileExistsError:
        raise SystemExit(f"Trial already started; refusing to replay {directory.name}. Collect its evidence and prepare a fresh run directory.")
    mirror = Mirror(directory)
    mirror.event("run.begin", manifest)
    snapshot(directory, "before")
    stop = threading.Event()
    startup = queue.Queue()
    logger = threading.Thread(target=stream_logs, args=(directory, stop, startup), daemon=True)
    logger.start()
    args = [str(binary)] + CASES[manifest["case"]] + ["--lab-journal", str(directory)]
    if manifest["case"] in ("finalization", "finalization-remaining"):
        args = [sys.executable, str(directory / "finalize_lab.py"), str(directory)]
    child = None
    try:
        await_kernel_recorder(startup)
        mirror.event("kernel.ready", "startup banner persisted locally and remotely")
        mirror.event("child.start", {"argv": args})
        with open(directory / "stdout.log", "ab", buffering=0) as output:
            child = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            def copy_output():
                for line in child.stdout:
                    output.write(line)
                    flush(output)
                    sys.stdout.write(line.decode(errors="replace"))
                    sys.stdout.flush()
            reader = threading.Thread(target=copy_output, daemon=True)
            reader.start()
            try:
                code = child.wait(timeout=210 if manifest["case"] in ("finalization", "finalization-remaining") else 60)
            except subprocess.TimeoutExpired:
                save(directory / "sample-result.json", json.dumps(command(["sample", str(child.pid), "1", "1", "-file", str(directory / "hang.sample.txt")]), indent=2))
                mirror.event("child.timeout", {"pid": child.pid})
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
                code = 124
            reader.join(timeout=5)
        mirror.event("child.exit", {"returncode": code})
        snapshot(directory, "after")
        save(directory / "result.json", json.dumps({"returncode": code, "completed_ns": time.time_ns()}))
        print(f"Trial finished: returncode={code}; evidence={directory}", flush=True)
        if code != 0:
            raise SystemExit("Trial failed; evidence retained. Do not rerun this directory; inspect the journal before preparing another trial.")
    finally:
        stop.set()
        logger.join(timeout=5)
        mirror.close()
        # Make diagnostic files readable by the developer inside a private 0700
        # run directory; never loosen the token or directory permissions.
        for path in directory.iterdir():
            if path.is_file() and path.name != "token":
                os.chmod(path, 0o644)


def collect(directory):
    directory = Path(directory).resolve()
    manifest = json.loads((directory / "manifest.json").read_text())
    recovery = directory / ("recovery-" + datetime.datetime.now().strftime("%Y%m%d-%H%M%S"))
    recovery.mkdir(mode=0o700)
    snapshot(recovery, "environment")
    reports = []
    for path in Path("/Library/Logs/DiagnosticReports").iterdir():
        if path.stat().st_mtime_ns >= manifest["prepared_ns"] and any(s in path.name.lower() for s in ["panic", "resetcounter", "knightbus", "shutdown"]):
            try:
                shutil.copy2(path, recovery / path.name)
                reports.append({"file": path.name, "sha256": digest(recovery / path.name)})
            except (PermissionError, IsADirectoryError) as error:
                reports.append({"file": path.name, "error": str(error)})
    save(recovery / "reports.json", json.dumps(reports, indent=2))
    try:
        subprocess.run(["scp", "-q", f"{manifest['remote']}:{manifest['remote_directory']}/mirror.jsonl", str(recovery / "remote-mirror.jsonl")], check=True, timeout=30)
    except (subprocess.SubprocessError, OSError) as error:
        save(recovery / "remote-copy-error.txt", str(error))
    # log show is read-only and may still recover events absent from the live
    # stream. Permission/retention failures are preserved, not treated as success.
    save(recovery / "unified-log.json", json.dumps(command(["/usr/bin/log", "show", "--last", "30m", "--style", "ndjson", "--predicate", 'process == "kernel" AND (eventMessage CONTAINS[c] "USB" OR eventMessage CONTAINS[c] "HID")'], timeout=60)))
    save(recovery / "collector.json", json.dumps({"script_sha256": digest(__file__), "collected_ns": time.time_ns()}))
    # A process exit can interrupt a callback after its local flush but before
    # remote delivery. Preserve that local tail and new panic reports on Linux
    # without rewriting the original live mirror or implying it acknowledged them.
    recovery_files = list(recovery.iterdir())
    archive_path = recovery / "recovery-and-local-logs.tar.gz"
    with tarfile.open(archive_path, "w:gz") as archive:
        for path in recovery_files:
            if path.is_file():
                archive.add(path, arcname="recovery/" + path.name)
        for pattern in ["native-*.jsonl", "supervisor-*.jsonl", "stdout.log", "kernel-usb-hid.jsonl",
                        "before.json", "after.json", "result.json", "STARTED", "kernel-recorder-error.txt",
                        "hang.sample.txt", "sample-result.json", "finalization.json", "launchd*.json",
                        "launchd*.log", "local-hid*.json", "normal-embedded.json", "unprivileged-capture.json",
                        "owner*.json", "owner.log", "temporary-launchd.plist"]:
            for path in directory.glob(pattern):
                if path.is_file():
                    archive.add(path, arcname="local/" + path.name)
    with open(archive_path, "rb") as file:
        os.fsync(file.fileno())
        if sys.platform == "darwin":
            fcntl.fcntl(file.fileno(), 51)
    remote_archive = manifest["remote_directory"] + "/" + recovery.name + ".tar.gz"
    try:
        subprocess.run(["scp", "-q", str(archive_path), f"{manifest['remote']}:{remote_archive}"], check=True, timeout=30)
        if remote(manifest["remote"], ["sha256sum", remote_archive]).split()[0] != digest(archive_path):
            raise RuntimeError("recovery archive hash mismatch")
        save(recovery / "remote-recovery.json", json.dumps({"path": remote_archive, "sha256": digest(archive_path)}))
    except (subprocess.SubprocessError, OSError, RuntimeError) as error:
        save(recovery / "remote-recovery-error.txt", str(error))
    print(recovery)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    p = sub.add_parser("prepare")
    p.add_argument("--repo", required=True)
    p.add_argument("--binary", required=True)
    p.add_argument("--evidence", required=True)
    p.add_argument("--remote", required=True)
    p.add_argument("--case", choices=CASES, default="capture-worker")
    for name in ["run", "collect", "sink", "start-sink"]:
        sub.add_parser(name).add_argument("directory")
    args = parser.parse_args()
    if args.action == "prepare":
        prepare(args)
    else:
        {"run": run, "collect": collect, "sink": sink, "start-sink": start_sink}[args.action](args.directory)


if __name__ == "__main__":
    main()
