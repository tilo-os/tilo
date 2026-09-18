"""Exercise the execution checker using retained live server records."""
import copy
import importlib.util
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).parent
SPEC = importlib.util.spec_from_file_location("audit_run", ROOT / "audit-run.py")
AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIT)


class RunEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.receipt = json.loads((ROOT / "results/misleading-status-20260918/successful-run-audit.json").read_text())

    def errors(self, receipt=None):
        receipt = receipt or self.receipt
        expected = receipt["expected"]
        return AUDIT.execution_errors(receipt["run"], receipt["jobs"], expected["repo"],
                                      expected["head"], expected["workflow"], expected["jobs"])

    def test_live_success(self):
        self.assertEqual([], self.errors())

    def test_live_misleading_status_does_not_hide_failed_job(self):
        path = ROOT / "results/misleading-status-20260918"
        statuses = json.loads((path / "statuses.json").read_text(encoding="utf-8-sig"))
        self.assertEqual("success", next(s["status"] for s in statuses["statuses"] if s["context"] == "pilot/ready"))
        failed = json.loads((path / "failed-run-audit.json").read_text())
        self.assertEqual(statuses["sha"], failed["run"]["commit_sha"])
        self.assertTrue(self.errors(failed))

    def test_identity_mismatches_fail(self):
        for key in ("commit_sha", "workflow_id", "repository"):
            with self.subTest(key=key):
                receipt = copy.deepcopy(self.receipt)
                receipt["run"][key] = {} if key == "repository" else "wrong"
                self.assertTrue(self.errors(receipt))

    def test_non_success_job_fails_even_when_run_green(self):
        for state in ("failure", "cancelled", "skipped", "waiting", "running", None):
            with self.subTest(state=state):
                receipt = copy.deepcopy(self.receipt)
                receipt["jobs"][0]["status"] = state
                self.assertTrue(self.errors(receipt))

    def test_missing_duplicate_and_unexpected_jobs_fail(self):
        for jobs in ([], self.receipt["jobs"] * 2, [{**self.receipt["jobs"][0], "name": "other"}]):
            receipt = copy.deepcopy(self.receipt)
            receipt["jobs"] = jobs
            self.assertTrue(self.errors(receipt))

    def test_foreign_job_fails(self):
        for key in ("run_id", "repo_id"):
            receipt = copy.deepcopy(self.receipt)
            receipt["jobs"][0][key] = -1
            self.assertTrue(self.errors(receipt))

    def test_missing_ids_do_not_match_each_other(self):
        receipt = copy.deepcopy(self.receipt)
        del receipt["run"]["repository"]["id"]
        del receipt["jobs"][0]["repo_id"]
        self.assertTrue(self.errors(receipt))
        receipt = copy.deepcopy(self.receipt)
        del receipt["jobs"][0]["id"]
        self.assertTrue(self.errors(receipt))


if __name__ == "__main__":
    unittest.main()
