"""Fetch and verify the unmodified usbip-win2 UDE package and matching source."""
import hashlib
from pathlib import Path
import subprocess
import tarfile
import tempfile
import urllib.request
import zipfile

VERSION = "0.9.8.1"
COMMIT = "55e1fa7f0c2157017b02dc1f3236e98169a535e4"
ARCHIVE_NAME = f"USBip-{VERSION}-x64.exe"
ARCHIVE_URL = f"https://github.com/vadimgrn/usbip-win2/releases/download/v.{VERSION}/{ARCHIVE_NAME}"
ARCHIVE_SHA256 = "38cad6d4432b52d5bb9409d9ad03b72fdffc4ada4cd3a48fbeca1a2752a8518a"
SOURCE_URL = f"https://codeload.github.com/vadimgrn/usbip-win2/tar.gz/{COMMIT}"
SOURCE_SHA256 = "8ede57738de77921695362741f69a310fd09e01c6e6fdf5b074f6442336553c2"
SOURCE_NAME = "usbip-win2-source.tar.gz"
UNPACKER_URL = "https://github.com/jrathlev/InnoUnpacker-Windows-GUI/releases/download/ui_2_2_13/innounp-2.zip"
UNPACKER_SHA256 = "f8e45c9676bebc7d7570108ab572673b541fcf7a7d3d24a04319ac6b5315b4e8"
DRIVER_PACKAGES = ("usbip2_ude", "usbip2_filter")
DRIVER_HASHES = {
    "usbip2_filter.cat": "f259d60bdb5ad9d7a976418bfc9156b788dfcec9b67c8d05d036eb03fb18ffe1",
    "usbip2_filter.inf": "b7fe01b04793d2f83cf3407601f2c835f1a10c2618e32a52c8fdef1da6b79bff",
    "usbip2_filter.sys": "b93a5fc3c23d56f9b49634742374e32f269e53f764caa2d5fcb43e3259f7eced",
    "usbip2_ude.cat": "84dd6cc5985857adf1514a9ee202210ff3ee1384c9be40fd49d6810e23e25f3a",
    "usbip2_ude.inf": "1ae9e8dc497929de1a6be1291ae1dca6a19e7fd6c0351e8c266f65dd91f15073",
    "usbip2_ude.sys": "abd4fc43dce40e027e4acc884cbfe92433e026206f6565c5ee5284a39441169a",
}
DOWNLOAD_TIMEOUT_SECONDS = 60
MAX_DOWNLOAD_BYTES = 64 * 1024 * 1024


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


def extract_driver(archive, destination, cache):
    """Unpack data only; never run the upstream installer or its applications."""
    unpacker = cache / "innounp-2.zip"
    download(UNPACKER_URL, unpacker, UNPACKER_SHA256)
    with tempfile.TemporaryDirectory(dir=cache) as temporary:
        temporary = Path(temporary)
        executable = temporary / "innounp.exe"
        with zipfile.ZipFile(unpacker) as package:
            executable.write_bytes(package.read("innounp.exe"))
        extracted = temporary / "driver"
        subprocess.run([str(executable), "-x", "-b", "-y", f"-d{extracted}", str(archive), "*usbip2*"], check=True)
        for name, expected in DRIVER_HASHES.items():
            original = extracted / "{tmp}" / name
            check(original, expected)
            (destination / name).write_bytes(original.read_bytes())


def prepare(destination, cache):
    cache = cache.resolve()
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / ARCHIVE_NAME
    source = cache / SOURCE_NAME
    download(ARCHIVE_URL, archive, ARCHIVE_SHA256)
    download(SOURCE_URL, source, SOURCE_SHA256)
    destination.mkdir(parents=True, exist_ok=True)
    extract_driver(archive, destination, cache)
    (destination / SOURCE_NAME).write_bytes(source.read_bytes())
    with tarfile.open(source, "r:gz") as package:
        license_file = package.extractfile(f"usbip-win2-{COMMIT}/LICENSE.txt")
        (destination / "COPYING").write_bytes(license_file.read())
    (destination / "NOTICE.txt").write_text(
        f"usbip-win2 {VERSION}, commit {COMMIT}\n"
        "Copyright the usbip-win2 contributors. BSD-2-Clause; see COPYING and source notices.\n"
        "The original INF, SYS and CAT are distributed without modification.\n"
        "Corresponding source and build files are included in usbip-win2-source.tar.gz.\n"
        f"Upstream: https://github.com/vadimgrn/usbip-win2/tree/{COMMIT}\n"
        "KnightBus communicates with this separately licensed driver through its device interface and loopback USB/IP.\n"
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
    for package in DRIVER_PACKAGES:
        catalog = str(source / f"{package}.cat")
        run(["signtool", "verify", "/kp", catalog], check=True)
        for suffix in ("inf", "sys"):
            run(["signtool", "verify", "/kp", "/c", catalog, str(source / f"{package}.{suffix}")], check=True)
