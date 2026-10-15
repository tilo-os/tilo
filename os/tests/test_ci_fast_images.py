"""Exercise the CI image smoke script without building or running an image."""
from pathlib import Path
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = "os/tests/ci_fast_images.sh"
FAST = "/archives/ci fast.oci.tar"
CONTROL = "/archives/ci control.oci.tar"


class CiFastImagesTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        script = self.root / SCRIPT
        script.parent.mkdir(parents=True)
        shutil.copyfile(ROOT / SCRIPT, script)
        runner = self.root / "tools/ci/publish/oci-run.sh"
        runner.parent.mkdir(parents=True)
        # Replace only the image boundary. The production shell script still
        # constructs and dispatches both commands, with its real strict mode.
        runner.write_text(
            f"exec {shlex.quote(sys.executable)} -c " + shlex.quote("""
import json, os, sys
with open(os.environ['CALL_LOG'], 'a') as log:
    log.write(json.dumps(sys.argv[1:]) + '\\n')
sys.exit(37 if sys.argv[1] == os.environ.get('FAIL_ARCHIVE') else 0)
""") + ' "$@"\n'
        )
        self.log = self.root / "calls.jsonl"

    def run_script(self, *archives, fail_archive=""):
        result = subprocess.run(
            ["bash", str(self.root / SCRIPT), *archives],
            cwd=self.root,
            env={"PATH": os.defpath, "CALL_LOG": str(self.log),
                 "FAIL_ARCHIVE": fail_archive},
            text=True, capture_output=True, timeout=10,
        )
        calls = [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []
        return result, calls

    def test_both_archives_receive_the_full_strict_smoke_commands(self):
        result, calls = self.run_script(FAST, CONTROL)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(calls), 2)
        for call, archive in zip(calls, (FAST, CONTROL), strict=True):
            self.assertEqual(call[:5], [archive, "bash", "-euo", "pipefail", "-c"])
            self.assertEqual(len(call), 6)
            syntax = subprocess.run(["bash", "-n", "-c", call[5]], capture_output=True, text=True)
            self.assertEqual(syntax.returncode, 0, syntax.stderr)
        fast, control = calls[0][5], calls[1][5]
        for probe in (
            ". /etc/os-release", "cargo --version", "rustc --version",
            "cargo fmt --version", "cargo deny --version", "cargo machete --version",
            "cargo hakari --version", "sccache --version", "bwrap --version",
            'python3 -c "import PIL"',
            'fc-match -f "%{family}" "Noto Sans" | grep -Fq "Noto Sans"',
            'test -n "$(find /etc/pki/rpm-gpg -maxdepth 1 -name "RPM-GPG-KEY-fedora-*-primary" -print -quit)"',
        ):
            with self.subTest(probe=probe):
                self.assertIn(probe, fast)
        self.assertIn("for tool in git python3 jq rg node gpg curl diff which; do", fast)
        self.assertIn("for tool in cc modinfo clang ld.lld rpmbuild rpmkeys systemd-escape fc-match bwrap; do", fast)
        self.assertEqual(fast.count('command -v "$tool"'), 2)
        for probe in ("docker --version", "docker buildx version", "cargo --version"):
            self.assertIn(probe, control)

    def test_failed_fast_image_stops_before_control(self):
        result, calls = self.run_script(FAST, CONTROL, fail_archive=FAST)
        self.assertEqual(result.returncode, 37, result.stderr)
        self.assertEqual([call[0] for call in calls], [FAST])

    def test_failed_control_image_fails_the_smoke(self):
        result, calls = self.run_script(FAST, CONTROL, fail_archive=CONTROL)
        self.assertEqual(result.returncode, 37, result.stderr)
        self.assertEqual([call[0] for call in calls], [FAST, CONTROL])

    def test_missing_archive_is_rejected_before_any_image_runs(self):
        for args in ((), (FAST,)):
            with self.subTest(args=args):
                result, calls = self.run_script(*args)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("usage:", result.stderr)
                self.assertEqual(calls, [])

    def test_publication_requires_smoking_both_built_archives(self):
        checks = {check["name"]: check for check in tomllib.loads((ROOT / "t.toml").read_text())["check"]}
        smoke = checks["image-ci-fast-smoke"]
        self.assertEqual(smoke["needs"], ["image-ci-fast", "image-ci-control"])
        self.assertEqual(shlex.split(smoke["run"]), [
            "bash", SCRIPT, "/target/publish/ci-fast/image.oci.tar",
            "/target/publish/ci-control/image.oci.tar",
        ])
        self.assertIn(SCRIPT, smoke["inputs"])
        self.assertIn("image-ci-fast-smoke", checks["publish-ci-fast"]["needs"])
        self.assertEqual(checks["publish-ci-fast"]["on"], ["main"])
        control = checks["image-ci-control"]
        self.assertIn("image-ci-fast", control["needs"])
        self.assertIn("CI_FAST_IMAGE=oci-archive:/target/publish/ci-fast/image.oci.tar", control["run"])


if __name__ == "__main__":
    unittest.main()
