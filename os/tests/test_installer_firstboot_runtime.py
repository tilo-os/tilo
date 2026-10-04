#!/usr/bin/env python3
"""Exercise the actual target handoff with an OS lacking all firstboot /usr files."""
from pathlib import Path
import os
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
BACKEND = ROOT / "os/usr/libexec/tilo-install"


class FirstbootRuntime(unittest.TestCase):
    def stage(self, source, target):
        backend = BACKEND.read_text()
        start = backend.index("stage_installed_firstboot_runtime() {")
        function = backend[start:backend.index("\n}\n", start) + 3]
        return subprocess.run(
            ["bash", "-eu", "-c", """
# The fixture cannot relabel host files; record every production label call.
label_installed_tilo_path() { printf '%s\n' "$2" >> "$LABEL_TRACE"; }
die() { printf '%s\n' "$*" >&2; exit 1; }
""" + function + '\nstage_installed_firstboot_runtime "$1" "$2"',
             "firstboot-fixture", str(source), str(target)],
            env={**os.environ, "LABEL_TRACE": str(target.parent / "labels")},
            text=True, capture_output=True,
        )

    def test_signed_target_without_firstboot_payload_gets_executable_handoff(self):
        with tempfile.TemporaryDirectory() as work:
            work = Path(work)
            source = work / "live"
            target = work / "target" / "etc"
            target.mkdir(parents=True)
            for relative in ("usr/libexec/tilo-firstboot-user",
                             "usr/lib/systemd/system/tilo-firstboot-user.service",
                             "usr/libexec/tilo-firstboot-autologin-cleanup",
                             "usr/lib/systemd/system/tilo-firstboot-autologin-cleanup.service"):
                out = source / relative
                out.parent.mkdir(parents=True, exist_ok=True)
                out.write_bytes((ROOT / "os" / relative).read_bytes())
            baseline = source / "usr/share/tilo/greetd-config.toml"
            baseline.parent.mkdir(parents=True)
            baseline.write_bytes((ROOT / "os/etc/greetd/config.toml").read_bytes())
            # An already seeded recovery key and unfinished OOBE are preserved.
            authorized = target / "ssh/authorized_keys/alice"
            authorized.parent.mkdir(parents=True)
            authorized.write_text("ssh-ed25519 fixture-key\n")
            result = self.stage(source, target)
            self.assertEqual(result.returncode, 0, result.stderr)
            helper = target / "tilo/firstboot-user"
            self.assertEqual(helper.read_bytes(), (source / "usr/libexec/tilo-firstboot-user").read_bytes())
            self.assertEqual(helper.stat().st_mode & 0o777, 0o644)
            unit = target / "systemd/system/tilo-firstboot-user.service"
            self.assertIn("ExecStart=/usr/bin/bash /etc/tilo/firstboot-user\n", unit.read_text())
            self.assertNotIn("ExecStart=/usr/libexec/", unit.read_text())
            enable = target / "systemd/system/multi-user.target.wants/tilo-firstboot-user.service"
            self.assertEqual(enable.resolve(), unit)
            greetd = (target / "systemd/system/greetd.service.d/10-tilo-firstboot-user.conf").read_text()
            self.assertIn("Requires=tilo-firstboot-user.service", greetd)
            self.assertIn("After=tilo-firstboot-user.service", greetd)
            self.assertIn("ExecStartPre=/usr/bin/bash /etc/tilo/firstboot-user --check-session", greetd)
            self.assertIn("After=tilo-firstboot-user.service", (target / "systemd/system/sshd.service.d/10-tilo-firstboot-user.conf").read_text())
            self.assertEqual(authorized.read_text(), "ssh-ed25519 fixture-key\n")
            self.assertFalse((target.parent / "usr").exists())
            cleanup = target / "tilo/firstboot-autologin-cleanup"
            self.assertIn("BASE_CONFIG=/etc/tilo/firstboot-greetd-config.toml", cleanup.read_text())
            self.assertEqual((target / "tilo/firstboot-greetd-config.toml").read_bytes(), baseline.read_bytes())
            cleanup_unit = target / "systemd/system/tilo-firstboot-autologin-cleanup.service"
            self.assertIn("ExecStart=/usr/bin/bash /etc/tilo/firstboot-autologin-cleanup", cleanup_unit.read_text())
            self.assertEqual((target / "systemd/system/graphical.target.wants/tilo-firstboot-autologin-cleanup.service").resolve(), cleanup_unit)
            self.assertIn("/etc/tilo/firstboot-user", (work / "target/labels").read_text())
            # Resume writes the same runtime, never duplicates enabling links.
            result = self.stage(source, target)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(enable.resolve(), unit)

    def test_session_check_rejects_missing_account_even_without_seed(self):
        with tempfile.TemporaryDirectory() as work:
            config = Path(work) / "greetd.toml"
            script = ROOT / "os/usr/libexec/tilo-firstboot-user"
            def check(text):
                config.write_text(text)
                return subprocess.run(["bash", "-c", 'source "$1"; check_firstboot_session_user "$2"',
                                       "session-check", str(script), str(config)], capture_output=True, text=True)
            result = check('[initial_session]\nuser = "tilo-fixture-missing-9472"\n')
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("account is unavailable", result.stderr)
            self.assertNotEqual(check('[initial_session]\ncommand = "shell"\n').returncode, 0)
            self.assertEqual(check('[initial_session]\nuser = "root"\n').returncode, 0)
            self.assertEqual(check('[default_session]\nuser = "greetd"\n').returncode, 0)

    def test_base_os_ships_native_firstboot_runtime(self):
        containerfile = (ROOT / "os/Containerfile").read_text()
        for path in ("os/usr/libexec/tilo-firstboot-user",
                     "os/usr/libexec/tilo-firstboot-autologin-cleanup",
                     "os/usr/lib/systemd/system/tilo-firstboot-user.service",
                     "os/usr/lib/systemd/system/tilo-firstboot-autologin-cleanup.service"):
            self.assertIn(path, containerfile)
        self.assertIn("COPY os/etc/greetd/config.toml /usr/share/tilo/greetd-config.toml", containerfile)

    def test_missing_live_helper_fails_before_target_mutation(self):
        with tempfile.TemporaryDirectory() as work:
            work = Path(work)
            target = work / "target/etc"
            target.mkdir(parents=True)
            result = self.stage(work / "missing-live", target)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("missing its first-boot account runtime", result.stderr)
            self.assertEqual(list(target.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
