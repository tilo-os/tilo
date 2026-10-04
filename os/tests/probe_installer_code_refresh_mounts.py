#!/usr/bin/env python3
"""Explicit root private-namespace bind/rollback integration probe.

Run: unshare -m --propagation private python3 os/tests/probe_installer_code_refresh_mounts.py
Missing privileges, mount support, or isolation are failures, never skips.
"""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]


class RealMountTransaction(unittest.TestCase):
    def setUp(self):
        path = ROOT / 'usr/libexec/tilo-installer-code-refresh.py'
        spec = importlib.util.spec_from_file_location('refresh_mount_probe', path)
        self.refresh = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.refresh)

    def test_real_bind_mount_transaction_and_rollback(self):
        import subprocess
        self.assertEqual(os.geteuid(), 0, "run this probe as root")
        self.assertNotEqual(os.readlink("/proc/self/ns/mnt"), os.readlink("/proc/1/ns/mnt"),
                            "run this probe under unshare -m --propagation private")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); state = root / 'state'
            files = {'usr/libexec/tilo-install': b'new first', 'usr/libexec/tilo-install-dev-ssh.py': b'new second'}
            targets = []
            for relative in files:
                target = root / relative; target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(b'old'); target.chmod(0o755); targets.append(target)
            command = self.refresh.command
            with patch.object(self.refresh, 'gate'), patch.object(self.refresh, 'BASE', state), \
                    patch.object(self.refresh, 'ROOT', root):
                try:
                    self.refresh.apply(files, {'sha256':'a'*64})
                    for target, content in zip(targets, files.values()):
                        self.assertEqual(target.read_bytes(), content)
                        self.assertEqual(target.stat().st_mode & 0o777, 0o755)
                        with self.assertRaises(OSError):
                            target.write_bytes(b'cannot write readonly bind')
                    def fail_second(*argv):
                        if argv[:2] == ('mount', '--bind') and argv[-1] == str(targets[1]):
                            raise ValueError('injected second-bind failure')
                        return command(*argv)
                    with patch.object(self.refresh, 'command', side_effect=fail_second):
                        with self.assertRaisesRegex(ValueError, 'injected second-bind failure'):
                            self.refresh.apply({key: b'attempt' for key in files}, {'sha256':'b'*64})
                    for target, content in zip(targets, files.values()):
                        self.assertEqual(target.read_bytes(), content)
                    self.assertEqual(json.loads((state / 'active.json').read_text())['sha256'], 'a'*64)
                finally:
                    for target in reversed(targets):
                        subprocess.run(['umount', str(target)], check=True, capture_output=True)


if __name__ == '__main__':
    unittest.main()
