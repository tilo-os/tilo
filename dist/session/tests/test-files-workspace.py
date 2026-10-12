"""Exercise the shipped Files entry point without identities, FUSE or a desktop."""
import importlib.util
import json
import os
import re
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location("files_service", ROOT / "dist/session/tilo-files-workspace.py")
SERVICE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SERVICE)


class FilesWorkspaceTest(unittest.TestCase):
    def test_missing_or_unauthorized_namespace_never_becomes_empty_mount(self):
        with tempfile.TemporaryDirectory() as temporary:
            env = {"HOME": temporary, "XDG_RUNTIME_DIR": str(Path(temporary) / "run")}
            cache, _, _, runtime = SERVICE.paths(env)
            self.assertEqual(SERVICE.paths(env | {"XDG_STATE_HOME": "", "XDG_DATA_HOME": "", "XDG_CONFIG_HOME": "", "TILO_IDENTITY_ROOT": ""}), SERVICE.paths(env))
            with mock.patch.object(SERVICE.subprocess, "run") as call:
                with self.assertRaises(ValueError):
                    SERVICE.command(env, "/fs", "/admin")
                call.assert_not_called()
            self.assertFalse(cache.exists())
            self.assertFalse(runtime.exists())
            cache.mkdir(parents=True)
            (cache / "namespace.bin").write_bytes(b"retained namespace")
            with mock.patch.object(SERVICE.subprocess, "run", side_effect=subprocess.CalledProcessError(1, ["/admin"])):
                with self.assertRaises(subprocess.CalledProcessError):
                    SERVICE.command(env, "/fs", "/admin")
            self.assertFalse((cache / "mount").exists())
            self.assertFalse(runtime.exists())
            self.assertEqual((cache / "namespace.bin").read_bytes(), b"retained namespace")

    def test_guarded_daemon_receives_the_same_paths_as_authority_check(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            env = {"HOME": str(root), "XDG_STATE_HOME": str(root / "state"), "XDG_RUNTIME_DIR": str(root / "run"), "TILO_IDENTITY_ROOT": str(root / "certified")}
            cache, identity, config, runtime = SERVICE.paths(env)
            cache.mkdir(parents=True)
            (cache / "namespace.bin").write_bytes(b"saved fixture")
            status = subprocess.CompletedProcess([], 0, json.dumps({"can_resume_existing": True}).encode())
            with mock.patch.object(SERVICE.subprocess, "run", side_effect=[None, status]) as call:
                argv, child = SERVICE.command(env, "/fs", "/admin")
            self.assertEqual(call.call_args_list[0].args[0], ["/admin", "files-workspace-check", "--identity-root", str(identity), "--cache-dir", str(cache)])
            self.assertEqual(argv[:3], ["/fs", "serve", "--require-existing-workspace"])
            self.assertEqual(argv[argv.index("--acl") + 1], str(config / "fs-capabilities.json"))
            self.assertEqual(child["TILO_AUTHORITY_SOCKET_DIR"], str(runtime))
            self.assertEqual(child["TILO_IDENTITY_ROOT"], str(identity))
            self.assertEqual(argv[argv.index("--blob-sock") + 1], str(runtime / "fs-blobs.sock"))
            self.assertEqual((cache / "mount").stat().st_mode & 0o777, 0o700)
            self.assertEqual(runtime.stat().st_mode & 0o777, 0o700)
            self.assertFalse((identity / "device-private").exists())

    def test_refused_preflight_and_relative_paths_create_nothing(self):
        with tempfile.TemporaryDirectory() as temporary:
            env = {"HOME": temporary, "XDG_RUNTIME_DIR": str(Path(temporary) / "run")}
            cache, _, _, runtime = SERVICE.paths(env)
            cache.mkdir(parents=True)
            (cache / "namespace.bin").write_bytes(b"saved fixture")
            status = subprocess.CompletedProcess([], 0, b'{"can_resume_existing": false}')
            with mock.patch.object(SERVICE.subprocess, "run", side_effect=[None, status]):
                with self.assertRaises(ValueError):
                    SERVICE.command(env, "/fs", "/admin")
            self.assertFalse((cache / "mount").exists())
            self.assertFalse(runtime.exists())
            with self.assertRaises(ValueError):
                SERVICE.paths(env | {"XDG_STATE_HOME": "relative"})

    def test_shipped_console_entry_point_does_not_expose_readiness_stdout(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            env = os.environ | {
                "HOME": str(root), "XDG_STATE_HOME": str(root / "state"),
                "XDG_DATA_HOME": str(root / "data"), "XDG_CONFIG_HOME": str(root / "config"),
                "XDG_RUNTIME_DIR": str(root / "run"), "TILO_IDENTITY_ROOT": str(root / "identity"),
            }
            fs = root / "fs"
            admin = root / "admin"
            fs.write_text("#!/usr/bin/env python3\nprint('{\"can_resume_existing\": true}')\n")
            admin.write_text("#!/usr/bin/env python3\nprint('readiness-stdout-must-stay-private')\n")
            fs.chmod(0o755)
            admin.chmod(0o755)
            argv = ["python3", str(ROOT / "dist/session/tilo-files-workspace.py"), "--dry-run", "--fs-binary", str(fs), "--admin-binary", str(admin)]
            missing = subprocess.run(argv, env=env, capture_output=True, text=True)
            self.assertEqual(missing.returncode, 78)
            cache, _, _, _ = SERVICE.paths(env)
            self.assertFalse(cache.exists())
            cache.mkdir(parents=True)
            (cache / "namespace.bin").write_bytes(b"isolated saved namespace")
            ready = subprocess.run(argv, env=env, capture_output=True, text=True)
            self.assertEqual(ready.returncode, 0, ready.stderr)
            self.assertEqual(json.loads(ready.stdout)[:3], [str(fs), "serve", "--require-existing-workspace"])
            self.assertNotIn("readiness-stdout-must-stay-private", ready.stdout + ready.stderr)

    def test_real_staging_requires_and_delivers_primary_files_only_when_requested(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binaries = root / "bin"
            binaries.mkdir()
            for name in ["shell", "tilo-launcher", "tilo-unlock-helper", "imager"]:
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
            for relative in ["bin/fs", "bin/tilo-relay-admin", "bin/tilo-enroll", "libexec/tilo-files-workspace"]:
                self.assertTrue(os.access(primary / relative, os.X_OK))
            unit = (primary / "systemd-user/tilo-files-workspace.service").read_text()
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
        for dependency in ["fuse3", "python3", "dbus-tools"]:
            self.assertRegex(spec, r"(?m)^Requires:\s+" + dependency + r"$")
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
