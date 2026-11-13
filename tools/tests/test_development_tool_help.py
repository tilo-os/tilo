import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class DevelopmentToolHelp(unittest.TestCase):
    def test_help_does_not_build_install_or_require_a_checkout(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            called = root / "called"
            for name in ("cargo", "git", "python3", "uname", "codesign"):
                command = bin_dir / name
                command.write_text('#!/bin/sh\nprintf "%s\\n" "$0" >> "$FORBIDDEN_CALLS"\nexit 99\n')
                command.chmod(0o755)
            env = {**os.environ, "PATH": str(bin_dir), "FORBIDDEN_CALLS": str(called)}
            bash = shutil.which("bash")
            commands = [
                [sys.executable, str(ROOT / "tools/dev/install-git-hooks.py"), "--help"],
                [bash, str(ROOT / "dist/rpm/build-rpm.sh"), "--help"],
                [bash, str(ROOT / "dist/rpm/build-rpm.sh"), "--no-cargo", "--help"],
            ]
            for command in commands:
                with self.subTest(tool=command[-2]):
                    result = subprocess.run(command, cwd=root, env=env, text=True, capture_output=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn("usage:", result.stdout.lower())
                    self.assertFalse(called.exists(), result.stdout)
                    self.assertFalse((root / ".git").exists())

    def test_hook_installer_rejects_unknown_options_before_git(self):
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run(
                [sys.executable, str(ROOT / "tools/dev/install-git-hooks.py"), "--typo"],
                cwd=directory, env={**os.environ, "PATH": ""}, text=True, capture_output=True,
            )
            self.assertEqual(result.returncode, 2)
            self.assertIn("unrecognized arguments", result.stderr)

if __name__ == "__main__":
    unittest.main()
