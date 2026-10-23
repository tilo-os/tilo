"""tools/ci/cloud-conformance.sh, which t's cloud-servers and cloud-live checks run: a layer
whose servers or secrets are missing, or that runs fewer tests than the suite lists, fails."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/ci/cloud-conformance.sh"
sys.path.insert(0, str(ROOT / "tools/ci"))
import t_matrix  # noqa: E402

LIVE = {
    "TILO_LIVE_WEBDAV_URL": "https://dav.example.invalid/",
    "TILO_LIVE_WEBDAV_USER": "ada",
    "TILO_LIVE_WEBDAV_PASSWORD": "fixture",
    "TILO_LIVE_NEXTCLOUD_URL": "https://cloud.example.invalid/",
    "TILO_LIVE_NEXTCLOUD_USER": "ada",
    "TILO_LIVE_NEXTCLOUD_PASSWORD": "fixture",
}

# A conformance binary that lists two live tests and reports $FAKE_PASSED of them passing.
FAKE_SUITE = textwrap.dedent("""\
    #!/bin/sh
    if [ "$2" = --list ]; then
      printf 'live_webdav::create: test\\nlive_nextcloud::create: test\\n\\n2 tests, 0 benchmarks\\n'
      exit 0
    fi
    printf '%s\\n' "$PWD" "$@" > "$FAKE_RECORD"
    printf 'test result: ok. %s passed; 0 failed; 0 ignored\\n' "$FAKE_PASSED"
    """)


class CloudConformance(unittest.TestCase):
    def run_live(self, passed: str, env: dict[str, str]) -> tuple[subprocess.CompletedProcess, Path]:
        work = Path(self.enterContext(tempfile.TemporaryDirectory()))
        suite = work / "conformance"
        suite.write_text(FAKE_SUITE)
        suite.chmod(0o755)
        record = work / "record"
        clean = {key: value for key, value in os.environ.items() if not key.startswith("TILO_LIVE_")}
        result = subprocess.run(
            ["bash", str(SCRIPT), "live", str(suite)],
            env={**clean, **env, "FAKE_PASSED": passed, "FAKE_RECORD": str(record)},
            capture_output=True, text=True, timeout=60,
        )
        return result, record

    def test_live_runs_every_listed_test_one_at_a_time_in_the_crate(self) -> None:
        result, record = self.run_live("2", LIVE)
        self.assertEqual(result.returncode, 0, result.stderr)
        cwd, *args = record.read_text().splitlines()
        self.assertEqual(Path(cwd).resolve(), (ROOT / "crates/system/cloud-source-dav").resolve())
        self.assertEqual(args, ["live_", "--nocapture", "--test-threads=1"])

    def test_live_fails_on_each_missing_or_empty_secret(self) -> None:
        for name in LIVE:
            for value in (None, ""):
                with self.subTest(secret=name, value=value):
                    env = {key: val for key, val in LIVE.items() if key != name}
                    if value is not None:
                        env[name] = value
                    result, record = self.run_live("2", env)
                    self.assertEqual(result.returncode, 1)
                    self.assertIn(name, result.stderr)
                    self.assertFalse(record.exists(), "the suite ran without its secrets")

    def test_fewer_passing_tests_than_listed_fail(self) -> None:
        for passed in ("1", "0"):
            with self.subTest(passed=passed):
                result, _ = self.run_live(passed, LIVE)
                self.assertEqual(result.returncode, 1)
                self.assertIn("expected 2 passing live_ tests", result.stderr)

    def test_the_checks_run_this_script_on_the_built_suite(self) -> None:
        checks = {check["name"]: check for check in t_matrix.checks(ROOT)}
        suite = "/target/vm/cloud-source-dav-conformance"
        self.assertIn(suite, checks["cloud-conformance-build"]["outputs"])
        for name, layer in (("cloud-servers", "servers"), ("cloud-live", "live")):
            with self.subTest(check=name):
                check = checks[name]
                self.assertIn(f"bash tools/ci/cloud-conformance.sh {layer} {suite}", check["run"])
                self.assertEqual(check["needs"], ["cloud-conformance-build"])
                self.assertEqual(check.get("isolation"), "vm")
                self.assertIn("tools/ci/cloud-conformance.sh", check["inputs"])
        self.assertIn("export TILO_REQUIRE_CLOUD_SERVERS=1\n    run_layer container_\n", SCRIPT.read_text())
        self.assertEqual(sorted(checks["cloud-live"]["secrets"]), sorted(LIVE))
        self.assertIs(checks["cloud-live"]["cache"], False)


if __name__ == "__main__":
    unittest.main()
