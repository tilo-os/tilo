"""The image build contract: every runtime a shipped app pins is shipped."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
CHECK = ROOT / "os/scripts/check-shipped-runtimes.sh"
ENGINE = "2026.10.10-chromium"
NATIVE = "2026.07+native." + "0" * 32


class ShippedRuntimeContract(unittest.TestCase):
    def run_check(self, *, shipped, verifies=True, manifest=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            package = root / "store/pkgs/so.tilo.browser-1.pkg"
            package.mkdir(parents=True)
            (package / "manifest.toml").write_text(manifest or (
                f'[package]\nruntime = "tilo-runtime/{NATIVE}"\n'
                f'optional-runtimes = [\n  "tilo-runtime/{ENGINE}",\n]\n'
            ))
            for version in shipped:
                bundle = root / "runtimes" / version
                bundle.mkdir(parents=True)
                (bundle / "bundle.toml").write_text("")
            tool = root / "bundle"
            tool.write_text(f"#!/bin/sh\nexit {0 if verifies else 1}\n")
            tool.chmod(0o755)
            return subprocess.run(
                ["bash", str(CHECK)],
                env={"PATH": os.defpath, "STORE": str(root / "store"),
                     "RUNTIME_ROOT": str(root / "runtimes"), "BUNDLE_BIN": str(tool)},
                capture_output=True, text=True,
            )

    def test_passes_when_the_native_and_engine_runtimes_ship(self):
        result = self.run_check(shipped=[NATIVE, ENGINE])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("2 runtime pin(s)", result.stderr)

    def test_fails_when_the_pinned_engine_is_missing(self):
        # The old engine version shipping instead of the pinned one is the
        # exact drift that left the Browser saying "not installed".
        result = self.run_check(shipped=[NATIVE, "2026.10.2-chromium"])
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"tilo-runtime/{ENGINE}", result.stderr)

    def test_fails_when_the_native_runtime_is_missing(self):
        result = self.run_check(shipped=[ENGINE])
        self.assertEqual(result.returncode, 1)
        self.assertIn(NATIVE, result.stderr)

    def test_fails_when_a_shipped_bundle_does_not_verify(self):
        result = self.run_check(shipped=[NATIVE, ENGINE], verifies=False)
        self.assertEqual(result.returncode, 1)
        self.assertIn("does not verify", result.stderr)

    def test_single_line_optional_runtimes_are_read(self):
        result = self.run_check(
            shipped=[ENGINE],
            manifest=f'[package]\noptional-runtimes = ["tilo-runtime/{ENGINE}"]\n',
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_the_image_build_runs_the_check_and_pins_the_engine_to_the_source(self):
        pack = (ROOT / "os/scripts/pack-apps.sh").read_text()
        self.assertIn("check-shipped-runtimes.sh", pack)
        containerfile = (ROOT / "os/Containerfile").read_text()
        self.assertIn('test "$pinned" = "${CHROMIUM_RUNTIME_VERSION}"', containerfile)


if __name__ == "__main__":
    unittest.main()
