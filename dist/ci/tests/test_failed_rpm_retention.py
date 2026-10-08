#!/usr/bin/env python3
"""Exercise production retention bounds and the real failure/cleanup boundary."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

CI = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("retention", CI / "retain-failed-rpms.py")
retention = importlib.util.module_from_spec(spec)
spec.loader.exec_module(retention)


class FailedRpmRetentionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        clean = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        subprocess.run(["git", "init", "-q", str(self.root)], env=clean, check=True)
        subprocess.run(["git", "-C", str(self.root), "-c", "user.name=t", "-c",
                        "user.email=t@t", "commit", "--allow-empty", "--no-gpg-sign",
                        "-qm", "fixture"], env=clean, check=True)
        self.sha = subprocess.check_output(["git", "-C", str(self.root), "rev-parse", "HEAD"],
                                           env=clean, text=True).strip()
        self.rpms = self.root / "dist/rpm/build/RPMS/x86_64"
        self.rpms.mkdir(parents=True)
        self.output = self.root / "artifacts/failed-rpms"
        self.environment = patch.dict(os.environ, GITHUB_SHA=self.sha, TILO_EXPECT_ARCH="amd64")
        self.environment.start()
        self.addCleanup(self.environment.stop)

    def test_provenance_and_content_survive_without_becoming_accepted_output(self):
        package = self.rpms / "tilo-session-2026.07-1.x86_64.rpm"
        package.write_bytes(b"fixture RPM")
        retention.retain(self.root, self.output, "lint-rpm", "64")
        self.assertEqual((self.output / package.name).read_bytes(), package.read_bytes())
        manifest = json.loads((self.output / "provenance.json").read_text())
        self.assertEqual(manifest["source_commit"], self.sha)
        self.assertEqual(manifest["validation"], "failed-unvalidated")
        self.assertEqual(manifest["failed_stage"], "lint-rpm")
        self.assertEqual(manifest["exit_status"], 64)
        self.assertEqual(manifest["packages"][0]["sha256"], hashlib.sha256(package.read_bytes()).hexdigest())
        self.assertFalse((self.output.parent / package.name).exists())

    def test_no_packages_create_no_artifact(self):
        retention.retain(self.root, self.output, "build-rpm", "1")
        self.assertFalse(self.output.exists())

    def test_wrong_source_symlink_and_storage_limits_fail_without_partial_output(self):
        with patch.dict(os.environ, GITHUB_SHA="0" * 40):
            with self.assertRaises(ValueError):
                retention.retain(self.root, self.output, "lint-rpm", "1")
        package = self.rpms / "tilo-session-1.rpm"
        package.symlink_to(self.root / ".git/HEAD")
        with self.assertRaises(ValueError):
            retention.retain(self.root, self.output, "lint-rpm", "1")
        package.unlink()
        with package.open("wb") as stream:
            stream.truncate(retention.MAX_BYTES + 1)
        with self.assertRaises(ValueError):
            retention.retain(self.root, self.output, "lint-rpm", "1")
        package.unlink()
        for index in range(4):
            (self.rpms / f"tilo-session-{index}.rpm").write_bytes(b"tiny")
        with self.assertRaises(ValueError):
            retention.retain(self.root, self.output, "lint-rpm", "1")
        self.assertFalse(self.output.exists())

    def test_actual_exit_trap_preserves_original_status_even_when_retention_fails(self):
        source = (CI / "run-package-job.sh").read_text()
        trap = source.split("retain_failed_rpms() {", 1)[1].split("trap retain_failed_rpms EXIT", 1)[0]
        helper = self.root / "dist/ci/retain-failed-rpms.py"
        helper.parent.mkdir(parents=True)
        helper.write_text("import sys; sys.exit(17)\n")
        result = subprocess.run(["bash", "-c", "set -euo pipefail\ndistro=fedora-44\n"
                                 "stage=lint-rpm\nretain_failed_rpms() {" + trap +
                                 "trap retain_failed_rpms EXIT\nexit 64\n"],
                                cwd=self.root, capture_output=True, text=True)
        self.assertEqual(result.returncode, 64)
        self.assertIn("could not be retained", result.stderr)

    def test_package_growth_during_copy_cannot_exceed_cap_or_leave_partial_artifacts(self):
        first = self.rpms / "tilo-session-1.rpm"
        second = self.rpms / "tilo-session-2.rpm"
        first.write_bytes(b"small")
        second.write_bytes(b"small")
        original_sha256 = hashlib.sha256

        def grow_after_admission():
            second.write_bytes(b"grew" * 10)
            return original_sha256()

        with patch.object(retention, "MAX_BYTES", 20), \
                patch.object(retention.hashlib, "sha256", side_effect=grow_after_admission):
            with self.assertRaisesRegex(ValueError, "grew beyond"):
                retention.retain(self.root, self.output, "lint-rpm", "1")
        self.assertFalse(self.output.exists())

    def test_failed_artifact_never_feeds_host_session_and_has_short_retention(self):
        workflow = (CI.parents[1] / ".forgejo/workflows/host-session-e2e.yml").read_text()
        failed = workflow.split("- name: Retain source-bound RPMs", 1)[1].split("  host-session:", 1)[0]
        self.assertIn("failure() && !inputs.rpm-artifact && !inputs.rpm-source-run", failed)
        self.assertIn("retention-days: 2", failed)
        self.assertIn("github.run_attempt", failed)
        self.assertIn("/failed-rpms/*", failed)
        accepted = workflow.split("  host-session:", 1)[1]
        self.assertNotIn("failed-fedora", accepted)


if __name__ == "__main__":
    unittest.main()
