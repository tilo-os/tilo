#!/usr/bin/env python3
"""Portable real-process regressions for the installed startup verifier.

These test the verifier, not production Imager or an installed Linux package.
Python 3 and a native C compiler are explicit test prerequisites.
"""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


HERE = Path(__file__).resolve().parent


class InstalledImagerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.work = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.work.cleanup)
        cls.executable = Path(cls.work.name) / "tilo-import-folders"
        subprocess.run(
            ["cc", "-Wall", "-Wextra", "-Werror", str(HERE / "fixtures/import-smoke.c"),
             "-o", str(cls.executable)], check=True,
        )

    def verify(self, mode="valid", executable=None, inherited=False):
        def open_descriptors():
            source = os.open(os.devnull, os.O_RDONLY)
            for fd in (3, 9, 57):
                os.dup2(source, fd, inheritable=True)

        return subprocess.run(
            [sys.executable, str(HERE / "verify-installed-imager.py"),
             str(executable or self.executable)],
            env={**os.environ, "TILO_IMPORT_FIXTURE_MODE": mode},
            preexec_fn=open_descriptors if inherited else None,
            close_fds=not inherited,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=20,
        )

    def test_accepts_receiver_two_and_smoke_zero(self):
        result = self.verify()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_closes_inherited_channel_and_other_descriptors(self):
        result = self.verify(inherited=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_receiver_zero(self):
        result = self.verify("receiver-zero")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("expected exit 2", result.stderr)

    def test_rejects_other_receiver_exit(self):
        result = self.verify("receiver-wrong")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("expected exit 2", result.stderr)

    def test_rejects_smoke_nonzero(self):
        result = self.verify("smoke-wrong")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("expected exit 0", result.stderr)

    def test_receiver_timeout_fails(self):
        result = self.verify("receiver-timeout")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--qos-connected-lease-receiver timed out", result.stderr)

    def test_smoke_timeout_fails(self):
        result = self.verify("smoke-timeout")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--tilo-package-smoke timed out", result.stderr)

    def test_missing_executable_fails(self):
        result = self.verify(executable=self.executable.with_name("absent"))
        self.assertNotEqual(result.returncode, 0)

    def test_relative_path_cannot_fall_back_to_path(self):
        result = self.verify(executable=Path("true"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("absolute installed executable", result.stderr)

    def test_fhs_relative_root_reaches_installed_startup_checks(self):
        # Run the actual FHS owner against a real layout. Deliberate startup
        # failures are portable; full ELF/library acceptance remains Linux-only.
        root = Path(self.work.name) / "relative payload"
        repo = HERE.parent.parent
        installed_files = {
            "dist/session/tilo-session": "usr/libexec/tilo-session",
            "dist/session/tilo-session-runner": "usr/libexec/tilo-session-runner",
            "dist/session/tilo-import-folders.desktop": "usr/share/applications/tilo-import-folders.desktop",
            "dist/session/tilo.desktop": "usr/share/wayland-sessions/tilo.desktop",
            "os/etc/pam.d/tilo-unlock": "etc/pam.d/tilo-unlock",
            "os/usr/share/xdg-desktop-portal/portals/tilo.portal": "usr/share/xdg-desktop-portal/portals/tilo.portal",
            "os/usr/share/xdg-desktop-portal/tilo-portals.conf": "usr/share/xdg-desktop-portal/tilo-portals.conf",
            "os/usr/lib/systemd/user/tilo-graphical-session.target": "usr/lib/systemd/user/tilo-graphical-session.target",
            "LICENSE": "usr/share/licenses/tilo-session/LICENSE",
        }
        for source, destination in installed_files.items():
            target = root / destination
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(repo / source, target)
        for destination in ("usr/bin/tilo-shell", "usr/bin/tilo-launcher",
                            "usr/bin/tilo-import-folders", "usr/libexec/tilo-unlock-helper"):
            target = root / destination
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(self.executable, target)
        for mode, expected in (
            ("receiver-wrong", "--qos-connected-lease-receiver: expected exit 2, got 1"),
            ("smoke-wrong", "--tilo-package-smoke: expected exit 0, got 2"),
        ):
            with self.subTest(mode=mode):
                result = subprocess.run(
                    ["bash", str(repo / "dist/verify-payload.sh"), root.name + "/"],
                    cwd=self.work.name,
                    env={**os.environ, "TILO_LIBEXEC_DIR": "/usr/libexec",
                         "TILO_IMPORT_FIXTURE_MODE": mode},
                    capture_output=True, text=True, timeout=20,
                )
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn(expected, result.stderr)


if __name__ == "__main__":
    unittest.main()
