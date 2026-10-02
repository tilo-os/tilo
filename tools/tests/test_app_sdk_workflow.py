"""Contract of .forgejo/workflows/app-sdk.yml."""

from __future__ import annotations

import pathlib
import unittest

import yaml

ROOT = pathlib.Path(__file__).resolve().parents[2]
WORKFLOW = yaml.safe_load((ROOT / ".forgejo/workflows/app-sdk.yml").read_text())
JOBS = WORKFLOW["jobs"]


class AppSdkWorkflow(unittest.TestCase):
    def test_every_pull_request_reports_policy_and_web(self) -> None:
        triggers = WORKFLOW.get("on") or WORKFLOW[True]
        self.assertNotIn("paths", triggers["pull_request"])
        self.assertIn("paths", triggers["push"])
        for job in ("policy", "builder"):
            self.assertIn("changes", JOBS[job]["needs"])
            self.assertEqual(JOBS[job]["if"], "needs.changes.outputs.relevant == 'true'")

    def test_jobs_test_the_same_main(self) -> None:
        for job in ("policy", "linux", "web"):
            merges = [step for step in JOBS[job]["steps"] if step.get("uses") == "./.forgejo/actions/merge-main"]
            self.assertEqual(len(merges), 1, job)
            self.assertEqual(merges[0]["with"]["main"], "${{ needs.changes.outputs.main }}", job)

    def test_macos_stays_disabled_until_a_runner_serves_it(self) -> None:
        self.assertIs(JOBS["macos"]["if"], False)


if __name__ == "__main__":
    unittest.main()
