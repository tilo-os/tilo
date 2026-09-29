#!/usr/bin/env python3
"""Exercise the snapshot preflight with a mock OCI runtime, without downloads."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


def restart_fixture(test):
    """Give the next subTest phase of a merged test the fresh fixture it had
    as its own test: tear down, run cleanups, reset instance state, set up."""
    test.tearDown()
    test.doCleanups()
    fresh = type(test)(test._testMethodName).__dict__
    keep = {key: test.__dict__[key] for key in ("_outcome", "_subtest") if key in test.__dict__}
    test.__dict__.clear()
    test.__dict__.update(fresh, **keep)
    test.setUp()


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
            # Point xtrace at source:line so a TimeoutExpired names the blocked step.
            "PS4": "+ ${BASH_SOURCE[0]}:${LINENO}: ",
        }
        if manifest is not None:
            environment["TEST_SNAPSHOT_PACKAGES"] = manifest.rstrip("\n")
        # A hang bound, not a speed check: the script only starts a dozen
        # short processes, which a loaded runner can stretch past 10 s.
        try:
            process = subprocess.run(
                ["bash", "-x", str(self.script), "runtime", "example@sha256:fixture"],
                env=environment, text=True, capture_output=True, timeout=120,
            )
        except subprocess.TimeoutExpired as expired:
            # Runtime call log + bash -x: which child finished, which command hung.
            # communicate() may leave TimeoutExpired stdout/stderr as bytes even
            # when text=True; decode so CI logs stay readable.
            def captured(blob):
                if blob is None:
                    return ""
                if isinstance(blob, bytes):
                    return blob.decode("utf-8", "replace")
                return blob

            reached = calls.read_text().splitlines() if calls.exists() else []
            self.fail(
                "snapshot check stalled after runtime calls "
                f"{reached}; stdout: {captured(expired.stdout)!r}; "
                f"xtrace/stderr: {captured(expired.stderr)!r}"
            )
        return process, calls.read_text().splitlines() if calls.exists() else []

    def test_native_architecture_checks(self):
        with self.subTest(phase="both_matching_architectures_reach_package_check"):
            for host, image in (("x86_64", "amd64"), ("aarch64", "arm64")):
                with self.subTest(host=host):
                    process, calls = self.run_check(host, image)
                    self.assertEqual(process.returncode, 0, process.stderr)
                    self.assertIn("cp", calls)
                    self.assertEqual(calls[-1], "rm")
        restart_fixture(self)
        with self.subTest(phase="wrong_architecture_rejected_before_manifest_read"):
            for host, image in (("x86_64", "arm64"), ("aarch64", "amd64")):
                with self.subTest(host=host):
                    process, calls = self.run_check(host, image)
                    self.assertNotEqual(process.returncode, 0)
                    self.assertIn("does not match native build host", process.stderr)
                    self.assertNotIn("cp", calls)
                    self.assertEqual(calls[-1], "rm")
        restart_fixture(self)
        with self.subTest(phase="unknown_native_architecture_rejected_without_runtime"):
            process, calls = self.run_check("riscv64", "riscv64")
            self.assertEqual(process.returncode, 2)
            self.assertIn("supported native build host", process.stderr)
            self.assertEqual(calls, [])
        restart_fixture(self)
        with self.subTest(phase="wifi_packages_required_in_each_native_snapshot"):
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
