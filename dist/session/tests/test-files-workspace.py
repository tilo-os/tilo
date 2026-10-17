"""Exercise the Files staging and packaging without identities, FUSE or a desktop.

The launch logic itself is `fs serve-primary`, tested against the real binary
in crates/system/fs/tests/workspace_startup.rs.
"""
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]


class FilesWorkspaceTest(unittest.TestCase):
    def test_real_staging_requires_and_delivers_primary_files_only_when_requested(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binaries = root / "bin"
            binaries.mkdir()
            for name in [
                "shell",
                "tilo-launcher",
                "tilo-unlock-helper",
                "tilo-flight-recorder",
                "tilo-session-report",
                "imager",
            ]:
                path = binaries / name
                path.write_text("#!/bin/sh\nexit 0\n")
                path.chmod(0o755)
            driver = ROOT / "dist/stage-payload.sh"
            generic = root / "generic"
            subprocess.run(["bash", str(driver), str(binaries), str(generic)], check=True, capture_output=True)
            for name in ["fs", "tilo-relay-admin", "tilo-enroll"]:
                self.assertFalse((generic / "bin" / name).exists())
            primary = root / "primary"
            result = subprocess.run(["bash", str(driver), str(binaries), str(primary), "--primary-files"], capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(primary.exists())
            for name in ["fs", "tilo-relay-admin"]:
                path = binaries / name
                path.write_text("#!/bin/sh\nexit 0\n")
                path.chmod(0o755)
            result = subprocess.run(["bash", str(driver), str(binaries), str(primary), "--primary-files"], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("tilo-enroll", result.stderr)
            self.assertFalse(primary.exists(), "missing enrollment must fail before staging")
            enroll = binaries / "tilo-enroll"
            enroll.write_text("#!/bin/sh\nexit 0\n")
            enroll.chmod(0o755)
            subprocess.run(["bash", str(driver), str(binaries), str(primary), "--primary-files"], check=True, capture_output=True)
            for relative in ["bin/fs", "bin/tilo-relay-admin", "bin/tilo-enroll"]:
                self.assertTrue(os.access(primary / relative, os.X_OK))
            self.assertFalse((primary / "libexec/tilo-files-workspace").exists())
            unit = (primary / "systemd-user/tilo-files-workspace.service").read_text()
            self.assertIn("\nExecStart=/usr/bin/fs serve-primary\n", unit)
            self.assertIn("StandardOutput=null", unit)
            for hidden_mount in ["PrivateTmp=yes", "PrivateIPC=yes", "ProtectSystem=strict", "ProtectHome=read-only"]:
                self.assertNotIn(hidden_mount, unit)
            self.assertIn("Wants=tilo-files-workspace.service", (primary / "systemd-user/tilo-graphical-session.target").read_text())
            self.assertNotIn("Wants=tilo-files-workspace.service", (generic / "systemd-user/tilo-graphical-session.target").read_text())

    def test_rpm_installs_and_owns_all_files_setup_tools(self):
        spec = (ROOT / "dist/rpm/tilo-session.spec").read_text()
        install = spec.split("\n%install\n", 1)[1].split("\n%check\n", 1)[0]
        manifest = spec.split("\n%files\n", 1)[1].split("\n%files ", 1)[0]
        names = ["fs", "tilo-relay-admin", "tilo-enroll"]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "bin").mkdir()
            buildroot = root / "installed"
            for name in names:
                payload = root / "bin" / name
                payload.write_text("#!/bin/sh\nexit 0\n")
                payload.chmod(0o755)
                lines = [line for line in install.splitlines()
                         if re.match(r"install\s+.*\sbin/" + re.escape(name) + r"\s", line)]
                self.assertEqual(len(lines), 1, name)
                command = lines[0].replace("%{buildroot}", str(buildroot)).replace("%{_bindir}", "/usr/bin")
                subprocess.run(["bash", "-c", command], cwd=root, check=True, capture_output=True)
                installed = buildroot / "usr/bin" / name
                self.assertTrue(os.access(installed, os.X_OK))
                self.assertEqual(installed.read_bytes(), payload.read_bytes())
                self.assertIn("%{_bindir}/" + name, manifest.splitlines())
        for dependency in ["fuse3", "dbus-tools"]:
            self.assertRegex(spec, r"(?m)^Requires:\s+" + dependency + r"$")
        self.assertNotRegex(spec, r"(?m)^Requires:\s+python", "the host session runs no Python")
        self.assertIn("Requires:       %{name}-apps = %{version}-%{release}", spec)
        self.assertNotRegex(spec, r"(?m)^AutoReqProv:\s*(no|0)\s*$")
        filters = re.findall(r"(?m)^%global __requires_exclude_from\s+(.+)$", spec)
        self.assertTrue(filters, "signed stores retain their existing exclusions")
        for pattern in filters:
            for name in names:
                self.assertIsNone(re.search(pattern, "/usr/bin/" + name),
                                  "host tooling must retain automatic ELF dependencies")


if __name__ == "__main__":
    unittest.main()
