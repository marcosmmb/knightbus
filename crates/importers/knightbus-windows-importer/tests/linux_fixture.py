#!/usr/bin/env python3
"""Original bounded CDC echo fixture for stock Linux usbip_host, not product code.

Run as root in an isolated VM. Requires dummy_hcd, libcomposite and USB/IP tools.
The named gadget must not already exist. SIGTERM/Ctrl-C cleans up only this fixture.
"""
import argparse
import ipaddress
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import termios
import time
import tty


def run(*args):
    return subprocess.run(args, check=True, timeout=10)


def stop(_signal, _frame):
    raise KeyboardInterrupt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--peer", required=True, type=ipaddress.IPv4Address)
    parser.add_argument("--port", type=int, default=43240)
    parser.add_argument("--seconds", type=int, default=90)
    parser.add_argument("--composite", action="store_true", help="two CDC functions")
    args = parser.parse_args()
    if os.geteuid() != 0 or not 1024 <= args.port <= 65535 or not 1 <= args.seconds <= 300:
        parser.error("root, an unprivileged TCP port and a 1..300 second duration are required")
    root = Path("/sys/kernel/config/usb_gadget/knightbus_m7")
    for module in ("libcomposite", "dummy_hcd", "usbip_host"):
        if not (Path("/sys/module") / module).exists():
            run("modprobe", module)
    udc = Path("/sys/class/udc/dummy_udc.0")
    if not udc.exists() or (udc / "function").read_text().strip():
        raise RuntimeError("dummy_udc.0 must exist and be unused")
    root.mkdir()  # Exclusive ownership; never replace another lab's gadget.
    bus = None
    daemon = None
    descriptors = []
    paths = {}
    firewall = False
    rule = ["-p", "tcp", "--dport", str(args.port), "!", "-s", str(args.peer),
            "-m", "comment", "--comment", "knightbus-m7", "-j", "REJECT"]
    functions = ["acm.first", "acm.second"] if args.composite else ["acm.first"]
    signal.signal(signal.SIGTERM, stop)
    try:
        values = {"idVendor": "0x1209", "idProduct": "0x0001", "bcdUSB": "0x0200",
                  "bcdDevice": "0x0100", "bDeviceClass": "0xef" if args.composite else "2",
                  "bDeviceSubClass": "2", "bDeviceProtocol": "1" if args.composite else "0",
                  "bMaxPacketSize0": "64", "max_speed": "full-speed"}
        for name, value in values.items():
            (root / name).write_text(value)
        config = root / "configs/c.1"
        config.mkdir()
        (config / "MaxPower").write_text("100")
        for name in functions:
            function = root / "functions" / name
            function.mkdir()
            (config / name).symlink_to(function)
        (root / "UDC").write_text("dummy_udc.0")
        deadline = time.monotonic() + 5
        while bus is None:
            devices = []
            for device in Path("/sys/bus/usb/devices").iterdir():
                if "dummy_hcd" not in str(device.resolve()) or not (device / "idVendor").exists():
                    continue
                if (device / "idVendor").read_text().strip() == "1209" and (device / "idProduct").read_text().strip() == "0001":
                    devices.append(device.name)
            if len(devices) == 1:
                bus = devices[0]
            elif devices or time.monotonic() >= deadline:
                raise RuntimeError("cannot uniquely identify the owned dummy USB device")
            else:
                time.sleep(.05)
        run("usbip", "bind", "-b", bus)
        run("iptables", "-I", "INPUT", *rule)
        firewall = True
        daemon = subprocess.Popen(["usbipd", "-4", "-t", str(args.port)])
        for name in functions:
            number = (root / "functions" / name / "port_num").read_text().strip()
            fd = os.open("/dev/ttyGS" + number, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
            descriptors.append(fd)
            paths[fd] = "/dev/ttyGS" + number
            tty.setraw(fd, termios.TCSANOW)
        time.sleep(.1)
        if daemon.poll() is not None:
            raise RuntimeError("stock usbipd failed to listen")
        print(json.dumps({"ready": True, "bus_id": bus, "port": args.port, "ports": len(functions)}), flush=True)
        deadline = time.monotonic() + args.seconds
        while time.monotonic() < deadline:
            if daemon.poll() is not None:
                raise RuntimeError("stock usbipd exited")
            readable, _, _ = select.select(descriptors, [], [], .1)
            for fd in readable:
                try:
                    data = os.read(fd, 4096)
                    if not data:
                        raise OSError("gadget tty hung up")
                    offset = 0
                    write_deadline = time.monotonic() + 2
                    while offset < len(data):
                        if time.monotonic() >= write_deadline:
                            raise RuntimeError("CDC echo write deadline")
                        if select.select([], [fd], [], .1)[1]:
                            try:
                                offset += os.write(fd, data[offset:])
                            except BlockingIOError:
                                continue
                    print(json.dumps({"echo_bytes": len(data)}), flush=True)
                except BlockingIOError:
                    continue
                except OSError:
                    # SET_CONFIGURATION/reset may hang up this tty generation.
                    # Reopen the same owned port; never replay incomplete data.
                    replacement = None
                    try:
                        replacement = os.open(paths[fd], os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
                        tty.setraw(replacement, termios.TCSANOW)
                        paths[replacement] = paths.pop(fd)
                        descriptors[descriptors.index(fd)] = replacement
                        os.close(fd)
                    except OSError:
                        if replacement is not None:
                            os.close(replacement)
                    time.sleep(.01)
    except KeyboardInterrupt:
        pass
    finally:
        # Attempt every owned cleanup even if an earlier OS operation fails.
        failures = []
        def clean(action):
            try:
                action()
            except Exception as error:
                failures.append(str(error))
        for fd in descriptors:
            clean(lambda: os.close(fd))
        if daemon is not None:
            daemon.terminate()
            try:
                daemon.wait(timeout=5)
            except subprocess.TimeoutExpired:
                daemon.kill()
                clean(lambda: daemon.wait(timeout=5))
        if bus is not None and (Path("/sys/bus/usb/devices") / bus / "driver").resolve().name == "usbip-host":
            clean(lambda: run("usbip", "unbind", "-b", bus))
        if (root / "UDC").read_text().strip():
            clean(lambda: (root / "UDC").write_text("\n"))
        for name in functions:
            clean(lambda: (root / "configs/c.1" / name).unlink(missing_ok=True))
            function = root / "functions" / name
            if function.exists():
                clean(function.rmdir)
        config = root / "configs/c.1"
        if config.exists():
            clean(config.rmdir)
        clean(root.rmdir)
        if firewall:
            clean(lambda: run("iptables", "-D", "INPUT", *rule))
        if failures:
            raise RuntimeError("Fixture cleanup incomplete: " + "; ".join(failures))
        print("Fixture removed", flush=True)


if __name__ == "__main__":
    main()
