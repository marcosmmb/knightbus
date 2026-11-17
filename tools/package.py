#!/usr/bin/env python3
"""Build the real applications into a portable platform directory and ZIP archive."""
import argparse
import hashlib
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))
import windows_driver

ROOT = Path(__file__).resolve().parents[1]
VERSION = "0.1.0"
MACOS_MINIMUM = "13.0"
DRIVER_FILES = tuple(windows_driver.DRIVER_HASHES)

def verify_driver(source):
    windows_driver.verify(source)


def run(*args, **kwargs):
    subprocess.run(args, check=True, cwd=ROOT, **kwargs)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--target-dir", required=True, type=Path)
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--driver-package", type=Path, help="Prepared pinned usbip-win package with source/notices (otherwise downloaded automatically); requires SignTool")
    args = parser.parse_args()
    output, target = args.output.resolve(), args.target_dir.resolve()
    if output.exists():
        parser.error("Output already exists; select a new directory to preserve prior artifacts")
    system = platform.system()
    if system not in ("Darwin", "Windows"):
        parser.error("Native packages currently support macOS and Windows")
    if args.driver_package:
        if system != "Windows": parser.error("--driver-package is only supported on Windows")
        verify_driver(args.driver_package.resolve())
    env = dict(os.environ, CARGO_TARGET_DIR=str(target))
    if not args.skip_build:
        run("cargo", "build", "--release", "--locked", "-p", "knightbus", "-p", "knightbusd", "--features", "knightbus-sdk/mdns", env=env)
        run("cargo", "build", "--release", "--locked", "--manifest-path", "applications/knightbus-desktop/Cargo.toml", env=env)
    output.mkdir(parents=True)
    release = target / "release"
    suffix = ".exe" if system == "Windows" else ""
    for name in ("knightbus", "knightbusd"):
        shutil.copy2(release / (name + suffix), output)
    if system == "Darwin":
        contents = output / "KnightBus.app" / "Contents"
        executable = contents / "MacOS"
        executable.mkdir(parents=True)
        shutil.copy2(release / "knightbus-desktop", executable)
        shutil.copy2(release / "knightbusd", executable)
        run("codesign", "--force", "--sign", "-", str(executable / "knightbusd"))
        info = dict(CFBundleName="KnightBus", CFBundleDisplayName="KnightBus", CFBundleIdentifier="io.knightbus.desktop", CFBundleExecutable="knightbus-desktop", CFBundlePackageType="APPL", CFBundleIconFile="KnightBus.icns", CFBundleVersion=VERSION, CFBundleShortVersionString=VERSION, LSMinimumSystemVersion=MACOS_MINIMUM, NSHighResolutionCapable=True)
        (contents / "Info.plist").write_bytes(plistlib.dumps(info))
        resources = contents / "Resources"
        resources.mkdir()
        shutil.copy2(ROOT / "applications/knightbus-desktop/icons/icon.icns", resources / "KnightBus.icns")
        # Finder/cloud folders can add metadata that codesign rejects. Clear it
        # only from this generated bundle; source files remain untouched.
        run("xattr", "-cr", str(output / "KnightBus.app"))
        # Local integrity signature only. Distribution requires Developer ID/notarization.
        run("codesign", "--force", "--sign", "-", str(output / "KnightBus.app"))
        for name in ("knightbus", "knightbusd"):
            run("codesign", "--force", "--sign", "-", str(output / name))
    else:
        shutil.copy2(release / "knightbus-desktop.exe", output / "knightbus-desktop.exe")
        if platform.machine().lower() not in ("amd64", "x86_64"):
            parser.error("The pinned signed USB/IP driver supports Windows x64 only")
        driver = output / "driver"
        if args.driver_package:
            shutil.copytree(args.driver_package.resolve(), driver)
        else:
            windows_driver.prepare(driver, target / "upstream-driver")
        for name in ("install-windows.ps1", "install-driver.ps1", "windows-installation.ps1", "uninstall-windows.ps1", "INSTALL.md"):
            shutil.copy2(ROOT / "tools/install" / name, output)
    for source in ("LICENSE", "docs/desktop-manager.md", "docs/standalone.md", "docs/architecture.md"):
        shutil.copy2(ROOT / source, output)
    def checksums():
        records = []
        for file in sorted(output.rglob("*")):
            if file.is_file() and file.name != "SHA256SUMS":
                records.append(f"{hashlib.sha256(file.read_bytes()).hexdigest()}  {file.relative_to(output).as_posix()}")
        (output / "SHA256SUMS").write_text("\n".join(records) + "\n")
    checksums()
    if system == "Windows":
        run("makensis", f"/DPAYLOAD={output}", f"/DOUTPUT={output / 'KnightBus-Setup-x64.exe'}",
            f"/DICON={ROOT / 'applications/knightbus-desktop/icons/icon.ico'}", str(ROOT / "tools/install/setup.nsi"))
        checksums()
    shutil.make_archive(str(output), "zip", output.parent, output.name)
    print(output.with_suffix(".zip"))

if __name__ == "__main__":
    main()
