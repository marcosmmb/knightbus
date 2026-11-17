"""Package completeness, pinned dependencies and Windows signature verification."""
import hashlib
import importlib.util
import io
import os
import platform
import subprocess
import plistlib
import re
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location("package", ROOT / "tools/package.py")
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)
driver = package.windows_driver

class PackageTests(unittest.TestCase):
    def test_mac_app_contains_the_companion_and_icon(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "build"
            release = target / "release"
            release.mkdir(parents=True)
            for name in ("knightbus", "knightbusd", "knightbus-desktop", "knightbus-launcher"):
                (release / name).write_bytes(name.encode())
            output = root / "package"
            args = ["package.py", "--skip-build", "--output", str(output), "--target-dir", str(target)]
            with patch("sys.argv", args), patch.object(package.platform, "system", return_value="Darwin"), patch.object(package, "run"):
                package.main()
            contents = output / "KnightBus.app" / "Contents"
            self.assertEqual((contents / "MacOS" / "knightbusd").read_bytes(), b"knightbusd")
            self.assertEqual((contents / "MacOS" / "knightbus-launcher").read_bytes(), b"knightbus-launcher")
            self.assertEqual((output / "knightbus-launcher").read_bytes(), b"knightbus-launcher")
            info = plistlib.loads((contents / "Info.plist").read_bytes())
            self.assertIn("NSRemovableVolumesUsageDescription", info)
            self.assertTrue((contents / "Resources" / info["CFBundleIconFile"]).is_file())

    def test_privileged_launcher_and_installer_agree_on_fixed_paths(self):
        paths = dict(re.findall(r'^pub const (\w+): &str = "([^"]+)";',
                               (ROOT / "crates/knightbus-sdk/src/macos.rs").read_text(), re.MULTILINE))
        launcher = dict(re.findall(r'^#define (\w+) "([^"]+)"',
                                  (ROOT / "applications/knightbusd/src/launcher.c").read_text(), re.MULTILINE))
        for native, shared in (("DAEMON_PATH", "DAEMON_FILE"), ("OWNER_PATH", "OWNER_FILE"),
                               ("STATE_PATH", "IDENTITY_DIR"), ("LOG_PATH", "LOG_FILE"),
                               ("ENDPOINT_PATH", "CONTROL_ENDPOINT")):
            self.assertEqual(launcher[native], paths[shared], native)

    @unittest.skipUnless(platform.system() == "Darwin", "macOS launcher")
    def test_launcher_rejects_unprivileged_execution_and_extra_arguments(self):
        no_permission, usage_error = 77, 64  # sysexits.h
        with tempfile.TemporaryDirectory() as temporary:
            launcher = str(Path(temporary) / "knightbus-launcher")
            subprocess.run(["clang", "-Wall", "-Wextra", "-Werror", f"-mmacosx-version-min={package.MACOS_MINIMUM}",
                            str(ROOT / "applications/knightbusd/src/launcher.c"),
                            "-lbsm", "-o", launcher], check=True)
            for arguments in (["--unknown"], ["--check", "extra"]):
                self.assertEqual(subprocess.run([launcher, *arguments]).returncode, usage_error)
            if os.geteuid() != 0:
                for arguments in ([], ["--check"]):
                    self.assertEqual(subprocess.run([launcher, *arguments]).returncode, no_permission)

    def test_pinned_files_and_source_are_required_before_signature_verification(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            with self.assertRaises(ValueError): driver.verify(source)
            for name in (*driver.DRIVER_HASHES, driver.SOURCE_NAME, "COPYING", "NOTICE.txt"):
                (source / name).write_bytes(name.encode())
            hashes = {name: hashlib.sha256(name.encode()).hexdigest() for name in driver.DRIVER_HASHES}
            source_hash = hashlib.sha256(driver.SOURCE_NAME.encode()).hexdigest()
            with patch.object(driver, "DRIVER_HASHES", hashes), patch.object(driver, "SOURCE_SHA256", source_hash):
                from unittest.mock import Mock
                command = Mock()
                driver.verify(source, command)
                self.assertEqual(command.call_count, 3)
                self.assertTrue(all("/kp" in call.args[0] for call in command.call_args_list))
                with self.assertRaises(RuntimeError): driver.verify(source, Mock(side_effect=RuntimeError("untrusted")))
                (source / "usbip_vhci_ude.sys").write_bytes(b"modified")
                with self.assertRaises(ValueError): driver.verify(source, command)

    def test_preparation_excludes_upstream_executables_and_test_certificates(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cache = root / "cache"
            cache.mkdir()
            archive = cache / f"usbip-win-{driver.VERSION}.zip"
            with zipfile.ZipFile(archive, "w") as package_file:
                for name in (*driver.DRIVER_HASHES, "usbip.exe", "usbip_test.pfx", "usbip_stub.sys"):
                    package_file.writestr(name, name.encode())
            source = cache / driver.SOURCE_NAME
            with tarfile.open(source, "w:gz") as source_file:
                member = tarfile.TarInfo(f"usbip-win-{driver.COMMIT}/LICENSE")
                contents = b"fixture license"
                member.size = len(contents)
                source_file.addfile(member, io.BytesIO(contents))
            destination = root / "driver"
            with patch.object(driver, "ARCHIVE_SHA256", hashlib.sha256(archive.read_bytes()).hexdigest()), patch.object(driver, "SOURCE_SHA256", hashlib.sha256(source.read_bytes()).hexdigest()), patch.object(driver, "verify"):
                driver.prepare(destination, cache)
            self.assertEqual({p.name for p in destination.iterdir()}, {*driver.DRIVER_HASHES, driver.SOURCE_NAME, "COPYING", "NOTICE.txt"})

    def test_windows_package_requires_driver_and_builds_installer(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "build"
            release = target / "release"
            release.mkdir(parents=True)
            names = ("knightbus.exe", "knightbusd.exe", "knightbus-desktop.exe")
            for name in names: (release / name).write_bytes(name.encode())
            output = root / "package"
            args = ["package.py", "--skip-build", "--output", str(output), "--target-dir", str(target)]
            with patch("sys.argv", args), patch.object(package.platform, "system", return_value="Windows"), patch.object(package.platform, "machine", return_value="AMD64"), patch.object(driver, "prepare") as prepare, patch.object(package, "run") as command:
                package.main()
                prepare.assert_called_once()
                self.assertTrue(any(call.args[0] == "makensis" for call in command.call_args_list))
            self.assertEqual(len({p.name.casefold() for p in output.glob("*.exe")}), len(names))
            self.assertTrue((output / "install-driver.ps1").is_file())
            self.assertTrue((output / "windows-installation.ps1").is_file())
            self.assertFalse((output / "DRIVER-NOT-INCLUDED.txt").exists())
            self.assertTrue(output.with_suffix(".zip").is_file())
            args[args.index(str(output))] = str(root / "failed")
            with patch("sys.argv", args), patch.object(package.platform, "system", return_value="Windows"), patch.object(package.platform, "machine", return_value="AMD64"), patch.object(driver, "prepare", side_effect=ValueError("driver unavailable")):
                with self.assertRaises(ValueError): package.main()
            self.assertFalse((root / "failed.zip").exists())

if __name__ == "__main__": unittest.main()
