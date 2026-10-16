"""The browser setup must not hide a missing library behind an installer error."""

from pathlib import Path
import os
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "tools/ci/ensure-browser-dependencies.sh"
PACKAGES = [
    "nss", "atk", "at-spi2-atk", "cups-libs", "libdrm", "libXcomposite",
    "libXdamage", "libXrandr", "mesa-libgbm", "pango", "alsa-lib", "libxkbcommon",
]


class BrowserDependencies(unittest.TestCase):
    def run_helper(self, installed, installer_status):
        with tempfile.TemporaryDirectory() as directory:
            commands = Path(directory)
            logs = commands / "logs"
            logs.mkdir()
            for name, status in (("rpm", 0 if installed else 1), ("dnf", installer_status)):
                command = commands / name
                command.write_text(
                    '#!/bin/sh\nprintf "%s\\n" "$@" > "$COMMAND_LOG/' + name + '"\n'
                    + f"exit {status}\n"
                )
                command.chmod(0o755)
            result = subprocess.run(
                ["bash", str(HELPER)],
                env={**os.environ, "PATH": str(commands) + ":" + os.environ["PATH"],
                     "COMMAND_LOG": str(logs)},
                capture_output=True, text=True, check=False,
            )
            calls = {name: (logs / name).read_text().splitlines()
                     for name in ("rpm", "dnf") if (logs / name).exists()}
            return result.returncode, calls

    def test_installed_dependencies_do_not_consult_installer(self):
        status, calls = self.run_helper(True, 42)
        self.assertEqual(status, 0)
        self.assertEqual(calls, {"rpm": ["-q", *PACKAGES]})

    def test_missing_dependencies_install_the_complete_required_set(self):
        status, calls = self.run_helper(False, 0)
        self.assertEqual(status, 0)
        self.assertEqual(calls["dnf"], ["install", "-y", *PACKAGES])

    def test_failed_installation_refuses_browser_setup(self):
        status, calls = self.run_helper(False, 42)
        self.assertEqual(status, 42)
        self.assertEqual(calls["dnf"], ["install", "-y", *PACKAGES])
