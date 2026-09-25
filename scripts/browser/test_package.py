"""A portable browser package must carry its native runtime launcher."""
import contextlib
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("browser_package", Path(__file__).with_name("package.py"))
package = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(package)


class PackageTests(unittest.TestCase):
    def package_fixture(self, root, system):
        """Use actual filesystem packaging; replace only external build/sign tools."""
        native = root / "target/chromium"
        if system == "Darwin":
            native /= "Browser Engine.app/Contents/MacOS"
        native.mkdir(parents=True)
        (native / "qos-chromium-host").write_bytes(b"chromium fixture")

        def external(command, **kwargs):
            if command[0] == "cargo":
                name = command[command.index("-p") + 1]
                if name == "shell":
                    self.assertIn("--no-default-features", command)
                    name = command[command.index("--bin") + 1]
                profile = command[command.index("--profile") + 1]
                target = root / "target" / ("debug" if profile == "dev" else profile)
                target.mkdir(parents=True, exist_ok=True)
                binary = target / name
                binary.write_bytes((name + " fixture").encode())
                binary.chmod(0o755)
            elif command[0] != "codesign":
                self.fail(f"Unexpected external operation: {command}")

        with patch.object(package, "ROOT", root), \
             patch.object(package.platform, "system", return_value=system), \
             patch.object(package.subprocess, "run", side_effect=external), \
             patch.object(package.sys, "argv", ["package.py", "--profile", "dev", "--skip-native-build"]), \
             contextlib.redirect_stdout(io.StringIO()):
            package.main()

    def test_macos_bundle_carries_executable_runtime_beside_browser(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.package_fixture(root, "Darwin")
            executables = root / "target/browser-package/Browser.app/Contents/MacOS"
            host = executables / "qos-runtime-mcp"
            self.assertTrue(host.is_file(), "packaged browser must not need PATH or Cargo for its runtime")
            self.assertEqual(host.read_bytes(), b"qos-runtime-mcp fixture")
            self.assertTrue(host.stat().st_mode & 0o111)
            self.assertEqual((executables / "browser").read_bytes(), b"browser fixture")

    def test_linux_package_carries_executable_runtime_beside_browser(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.package_fixture(root, "Linux")
            executables = root / "target/browser-package/qos-browser"
            host = executables / "qos-runtime-mcp"
            self.assertTrue(host.is_file(), "packaged browser must not need PATH or Cargo for its runtime")
            self.assertEqual(host.read_bytes(), b"qos-runtime-mcp fixture")
            self.assertTrue(host.stat().st_mode & 0o111)
            self.assertEqual((executables / "browser").read_bytes(), b"browser fixture")


if __name__ == "__main__":
    unittest.main()
