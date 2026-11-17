"""Fetch and verify the unmodified usbip-win UDE package and matching source."""
import hashlib
from pathlib import Path
import subprocess
import tarfile
import urllib.request
import zipfile

VERSION = "0.3.6-dev"
COMMIT = "0d93d7459ea79c07f06cf7bef7da0a595bad4219"
ARCHIVE_URL = f"https://github.com/cezanne/usbip-win/releases/download/v{VERSION}/usbip-win-{VERSION}.zip"
ARCHIVE_SHA256 = "489daccb3ddffb1ec83ab6e0a4f56a8b13a9af34ca4f9bf445357053aad46dc2"
SOURCE_URL = f"https://codeload.github.com/cezanne/usbip-win/tar.gz/{COMMIT}"
SOURCE_SHA256 = "2184d1c3fb2d5c916fd8de56d3dd2931bb4078fb00d80a200567b570aa0c68a9"
SOURCE_NAME = "usbip-win-source.tar.gz"
DRIVER_HASHES = {
    "usbip_vhci_ude.inf": "869c237cd2a3d6fb5e98adb59f0f415d696614e99502896a856dd0362093b8b2",
    "usbip_vhci_ude.sys": "c04ae09e411870ba4f515b8099aaa05c7ae46eb889ccae0bacb90b089c266b76",
    "usbip_vhci_ude.cat": "a0d905c5faa1849052d79e421e058108ffa074f5acbf4a467ce7716dcbc0accc",
}
DOWNLOAD_TIMEOUT_SECONDS = 60
MAX_DOWNLOAD_BYTES = 16 * 1024 * 1024


def check(path, expected):
    if hashlib.sha256(path.read_bytes()).hexdigest() != expected:
        raise ValueError(f"Unrecognized or modified upstream file: {path.name}")


def download(url, path, expected):
    if not path.exists():
        with urllib.request.urlopen(url, timeout=DOWNLOAD_TIMEOUT_SECONDS) as response:
            data = response.read(MAX_DOWNLOAD_BYTES + 1)
        if len(data) > MAX_DOWNLOAD_BYTES:
            raise ValueError("Upstream download exceeded its size limit")
        if hashlib.sha256(data).hexdigest() != expected:
            raise ValueError(f"Upstream download checksum mismatch: {url}")
        path.write_bytes(data)
    check(path, expected)


def prepare(destination, cache):
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / f"usbip-win-{VERSION}.zip"
    source = cache / SOURCE_NAME
    download(ARCHIVE_URL, archive, ARCHIVE_SHA256)
    download(SOURCE_URL, source, SOURCE_SHA256)
    destination.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(archive) as package:
        # Never extract executables, the unsigned exporter, or the test certificate.
        for name in DRIVER_HASHES:
            (destination / name).write_bytes(package.read(name))
    (destination / SOURCE_NAME).write_bytes(source.read_bytes())
    with tarfile.open(source, "r:gz") as package:
        license_file = package.extractfile(f"usbip-win-{COMMIT}/LICENSE")
        (destination / "COPYING").write_bytes(license_file.read())
    (destination / "NOTICE.txt").write_text(
        f"usbip-win {VERSION}, commit {COMMIT}\n"
        "Copyright the usbip-win contributors. Licensed under GPL-3.0; see COPYING.\n"
        "The original INF, SYS and CAT are distributed without modification.\n"
        "Corresponding source and build files are included in usbip-win-source.tar.gz.\n"
        f"Upstream: https://github.com/cezanne/usbip-win/tree/{COMMIT}\n"
        "KnightBus communicates with this separately licensed driver through its device interface.\n"
    )
    verify(destination)


def verify(source, run=subprocess.run):
    for name, expected in DRIVER_HASHES.items():
        path = source / name
        if not path.is_file():
            raise ValueError(f"Driver package is missing {name}")
        check(path, expected)
    check(source / SOURCE_NAME, SOURCE_SHA256)
    for name in ("COPYING", "NOTICE.txt"):
        if not (source / name).is_file():
            raise ValueError(f"Driver package is missing {name}")
    catalog = str(source / "usbip_vhci_ude.cat")
    run(["signtool", "verify", "/kp", catalog], check=True)
    for name in ("usbip_vhci_ude.inf", "usbip_vhci_ude.sys"):
        run(["signtool", "verify", "/kp", "/c", catalog, str(source / name)], check=True)
