#!/usr/bin/env python3
"""Exercise the snapshot preflight with a mock OCI runtime, without downloads."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class SnapshotArchitectureTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        scripts = self.root / "os/dev"
        scripts.mkdir(parents=True)
        original = Path(__file__).resolve().parents[1] / "dev/verify-rpm-snapshot-package-set.sh"
        self.script = scripts / original.name
        self.script.write_text(original.read_text())
        self.executable(scripts / "derive-rpm-package-set.py", "#!/bin/sh\nprintf 'example-package\\n'\n")
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.executable(self.bin / "uname", '#!/bin/sh\nprintf "%s\\n" "$TEST_HOST_ARCH"\n')
        self.executable(self.bin / "runtime", '''#!/bin/sh
set -eu
printf '%s\n' "$1" >> "$TEST_CALLS"
case "$1" in
 create) printf 'test-container\n';;
 image) printf '%s\n' "$TEST_IMAGE_ARCH";;
 cp) printf '%s\n' "${TEST_SNAPSHOT_PACKAGES:-example-package}" > "$3";;
 rm) :;;
 *) exit 2;;
esac
''')

    @staticmethod
    def executable(path, text):
        path.write_text(text)
        path.chmod(0o755)

    def run_check(self, host, image, manifest=None):
        calls = self.root / "calls"
        environment = os.environ | {
            "PATH": f"{self.bin}:{os.environ['PATH']}",
            "TEST_HOST_ARCH": host, "TEST_IMAGE_ARCH": image,
            "TEST_CALLS": str(calls), "TMPDIR": str(self.root / "scratch"),
        }
        if manifest is not None:
            environment["TEST_SNAPSHOT_PACKAGES"] = manifest.rstrip("\n")
        process = subprocess.run(
            ["bash", str(self.script), "runtime", "example@sha256:fixture"],
            env=environment, text=True, capture_output=True, timeout=10,
        )
        return process, calls.read_text().splitlines() if calls.exists() else []

    def test_both_matching_architectures_reach_package_check(self):
        for host, image in (("x86_64", "amd64"), ("aarch64", "arm64")):
            with self.subTest(host=host):
                process, calls = self.run_check(host, image)
                self.assertEqual(process.returncode, 0, process.stderr)
                self.assertIn("cp", calls)
                self.assertEqual(calls[-1], "rm")

    def test_wrong_architecture_rejected_before_manifest_read(self):
        for host, image in (("x86_64", "arm64"), ("aarch64", "amd64")):
            with self.subTest(host=host):
                process, calls = self.run_check(host, image)
                self.assertNotEqual(process.returncode, 0)
                self.assertIn("does not match native build host", process.stderr)
                self.assertNotIn("cp", calls)
                self.assertEqual(calls[-1], "rm")

    def test_unknown_native_architecture_rejected_without_runtime(self):
        process, calls = self.run_check("riscv64", "riscv64")
        self.assertEqual(process.returncode, 2)
        self.assertIn("supported native build host", process.stderr)
        self.assertEqual(calls, [])

    def test_wifi_packages_required_in_each_native_snapshot(self):
        derive = Path(__file__).resolve().parents[1] / "dev/derive-rpm-package-set.py"
        for host, image in (("x86_64", "amd64"), ("aarch64", "arm64")):
            with self.subTest(host=host):
                packages = subprocess.check_output(
                    [sys.executable, str(derive), "--arch", host], text=True,
                ).splitlines()
                required = {"NetworkManager-wifi", "wireless-regdb"}
                self.assertTrue(required.issubset(packages))
                manifest = "\n".join(packages) + "\n"
                # Feed the real architecture-specific consumer set to the
                # runtime fixture, then exercise the production stale-snapshot
                # check independently for each missing wireless dependency.
                self.executable(
                    self.script.parent / derive.name,
                    "#!/bin/sh\ncat <<'PACKAGES'\n" + manifest + "PACKAGES\n",
                )
                process, _ = self.run_check(host, image, manifest)
                self.assertEqual(process.returncode, 0, process.stderr)
                for missing in sorted(required):
                    with self.subTest(missing=missing):
                        stale = "\n".join(p for p in packages if p != missing)
                        process, calls = self.run_check(host, image, stale)
                        self.assertNotEqual(process.returncode, 0)
                        self.assertIn("does not match the current Containerfiles", process.stderr)
                        self.assertIn(missing, process.stdout)
                        self.assertEqual(calls[-1], "rm")


if __name__ == "__main__":
    unittest.main()
