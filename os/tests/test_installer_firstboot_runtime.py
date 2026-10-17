#!/usr/bin/env python3
"""Exercise the actual target handoff with an OS lacking all firstboot /usr files."""
from pathlib import Path
import os
import shutil
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
""" + function + '\nPREPARED_SETUP_RUN="$1/no-prepared-setup"\nstage_installed_firstboot_runtime "$1" "$2"',
             "firstboot-fixture", str(source), str(target)],
            env={**os.environ, "LABEL_TRACE": str(target.parent / "labels")},
            text=True, capture_output=True,
        )

    def test_signed_target_without_firstboot_payload_gets_executable_handoff(self):
        with tempfile.TemporaryDirectory() as work:
            work = Path(work).resolve()
            source = work / "live"
            target = work / "target" / "etc"
            target.mkdir(parents=True)
            # The compiled file helper is a build output, not a repository
            # file; the handoff copies whatever bytes the live image carries.
            helper_files = source / "usr/libexec/tilo-firstboot-files"
            helper_files.parent.mkdir(parents=True)
            files_binary = Path(os.environ.get('TILO_FIRSTBOOT_FILES_BINARY',
                                ROOT / 'target/debug/tilo-firstboot-files'))
            self.assertEqual(files_binary.read_bytes()[:4], b'\x7fELF', str(files_binary))
            shutil.copyfile(files_binary, helper_files)
            helper_files.chmod(0o755)
            for relative in ("usr/libexec/tilo-firstboot-user",
                             "usr/libexec/tilo-firstboot-home-seeds",
                             "usr/libexec/tilo-installer-prepared-setup-extract",
                             "usr/lib/systemd/system/tilo-firstboot-user.service",
                             "usr/libexec/tilo-firstboot-autologin-cleanup",
                             "usr/lib/systemd/system/tilo-firstboot-autologin-cleanup.service",
                             "usr/libexec/tilo-session-recovery",
                             "usr/lib/systemd/system/tilo-session-recovery.service"):
                out = source / relative
                out.parent.mkdir(parents=True, exist_ok=True)
                if out.name in ("tilo-firstboot-home-seeds", "tilo-installer-prepared-setup-extract"):
                    override = {"tilo-firstboot-home-seeds": "TILO_FIRSTBOOT_HOME_SEEDS_BINARY",
                                "tilo-installer-prepared-setup-extract": "TILO_PREPARED_EXTRACT_BINARY"}[out.name]
                    binary = Path(os.environ.get(override, ROOT / "target/debug" / out.name))
                    self.assertEqual(binary.read_bytes()[:4], b"\x7fELF", str(binary))
                    shutil.copyfile(binary, out)
                    out.chmod(0o755)
                else:
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
            files_helper = target / "tilo/tilo-firstboot-files"
            self.assertEqual(files_helper.read_bytes(),
                             (source / "usr/libexec/tilo-firstboot-files").read_bytes())
            # The compiled helper is run directly by the staged firstboot-user.
            self.assertEqual(files_helper.stat().st_mode & 0o777, 0o755)
            self.assertIn("/etc/tilo/tilo-firstboot-files", (work / "target/labels").read_text())
            self.assertEqual(helper.stat().st_mode & 0o777, 0o644)
            self.assertFalse((target / "tilo/tilo-firstboot-files.py").exists())
            seeds = target / "tilo/firstboot-home-seeds"
            self.assertEqual(seeds.read_bytes(), (source / "usr/libexec/tilo-firstboot-home-seeds").read_bytes())
            self.assertEqual(seeds.stat().st_mode & 0o777, 0o755)
            self.assertEqual(subprocess.run([str(seeds), "--capabilities"], capture_output=True).returncode, 0)
            transport = target / "tilo/prepared-setup-transport"
            self.assertEqual(transport.read_bytes(), (source / "usr/libexec/tilo-installer-prepared-setup-extract").read_bytes())
            self.assertEqual(transport.stat().st_mode & 0o777, 0o755)
            self.assertNotEqual(subprocess.run([str(transport), "invalid-operation"], capture_output=True).returncode, 0)
            self.assertIn("/etc/tilo/firstboot-home-seeds", (work / "target/labels").read_text())
            unit = target / "systemd/system/tilo-firstboot-user.service"
            self.assertIn("ExecStart=/usr/bin/bash /etc/tilo/firstboot-user\n", unit.read_text())
            self.assertNotIn("ExecStart=/usr/libexec/", unit.read_text())
            self.assertIn("OnFailure=tilo-session-recovery.service", unit.read_text())
            enable = target / "systemd/system/multi-user.target.wants/tilo-firstboot-user.service"
            self.assertEqual(enable.resolve(), unit)
            greetd = (target / "systemd/system/greetd.service.d/10-tilo-firstboot-user.conf").read_text()
            self.assertIn("Requires=tilo-firstboot-user.service", greetd)
            self.assertIn("OnFailure=tilo-session-recovery.service", greetd)
            recovery_unit = target / "systemd/system/tilo-session-recovery.service"
            self.assertIn("ExecStart=/usr/bin/bash /etc/tilo/session-recovery", recovery_unit.read_text())
            self.assertIn("TTYPath=/dev/tty2", recovery_unit.read_text())
            self.assertIn("Conflicts=getty@tty2.service greetd.service", recovery_unit.read_text())
            self.assertIn("Wants=getty@tty3.service", recovery_unit.read_text())
            self.assertIn("NoNewPrivileges=yes", recovery_unit.read_text())
            self.assertTrue((target / "tilo/session-recovery").is_file())
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

    def test_failed_startup_displays_recovery_and_filters_secret_output(self):
        with tempfile.TemporaryDirectory() as work:
            work = Path(work)
            commands = work / "bin"
            commands.mkdir()
            for name, text in {
                "plymouth": 'echo "plymouth $*" >> "$TRACE"; exit 1',
                "chvt": 'echo "chvt $*" >> "$TRACE"',
                "systemctl": '[ "$1" != is-active ] || exit 0; echo "ActiveState=failed"',
                "journalctl": 'echo "password=DO_NOT_DISPLAY_PRIVATE_SECRET"; echo "greetd.service: Failed with result exit-code."',
            }.items():
                script = commands / name
                script.write_text("#!/bin/bash\n" + text + "\n")
                script.chmod(0o755)
            result = subprocess.run(
                ["bash", "-c", 'source "$1"; show_session_recovery "$2"', "recovery-fixture",
                 str(ROOT / "os/usr/libexec/tilo-session-recovery"), str(commands)],
                env={**os.environ, "TRACE": str(work / "trace")}, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("Tilo could not start the desktop", result.stdout)
            self.assertIn("Recovery SSH is running", result.stdout)
            self.assertIn("authenticated console login", result.stdout)
            self.assertIn("ActiveState=failed", result.stdout)
            self.assertIn("greetd.service: Failed with result", result.stdout)
            self.assertNotIn("PRIVATE_SECRET", result.stdout)
            self.assertEqual((work / "trace").read_text(), "plymouth quit\nchvt 2\n")
            # A status-only fallback cannot turn failure into a privileged login.
            script = (ROOT / "os/usr/libexec/tilo-session-recovery").read_text()
            self.assertNotIn("sulogin", script)
            self.assertNotIn("bash -i", script)
            self.assertNotIn("setenforce", script)

    def test_base_os_ships_native_firstboot_runtime(self):
        containerfile = (ROOT / "os/Containerfile").read_text()
        for path in ("os/usr/libexec/tilo-firstboot-user",
                     "target/release/tilo-firstboot-files",
                     "target/release/tilo-firstboot-home-seeds",
                     "target/release/tilo-installer-prepared-setup-extract",
                     "os/usr/libexec/tilo-firstboot-autologin-cleanup",
                     "os/usr/lib/systemd/system/tilo-firstboot-user.service",
                     "os/usr/lib/systemd/system/tilo-firstboot-autologin-cleanup.service"):
            self.assertIn(path, containerfile)
        self.assertIn("COPY os/etc/greetd/config.toml /usr/share/tilo/greetd-config.toml", containerfile)

    def test_missing_seed_helper_fails_before_target_mutation(self):
        with tempfile.TemporaryDirectory() as work:
            work = Path(work)
            source = work / "live"
            for relative in ("usr/libexec/tilo-firstboot-user", "usr/lib/systemd/system/tilo-firstboot-user.service"):
                output = source / relative
                output.parent.mkdir(parents=True, exist_ok=True)
                output.write_bytes((ROOT / "os" / relative).read_bytes())
            target = work / "target/etc"
            target.mkdir(parents=True)
            result = self.stage(source, target)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("missing its first-boot account runtime", result.stderr)
            self.assertEqual(list(target.iterdir()), [])

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
