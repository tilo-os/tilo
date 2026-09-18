"""Regression coverage for read-only integration evidence, using real Git objects."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("readiness", Path(__file__).parents[1] / "ci_integration_readiness.py")
AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIT)
HEAD, BASE = "a" * 40, "b" * 40


def fixture():
    policy = {"trusted_revision": BASE, "protected_paths": [".forgejo", "tools"],
              "reviewers": ["reviewer"], "runs": [{"run_id": 12, "workflow": "check.yml",
                                                   "events": ["pull_request"], "jobs": ["check"]}]}
    review = {"id": 4, "user": {"login": "reviewer"}, "state": "APPROVED",
              "commit_id": HEAD, "official": True, "dismissed": False}
    run = {"id": 12, "repository": {"id": 7, "full_name": "qos/qos"}, "commit_sha": HEAD,
           "workflow_id": "check.yml", "event": "pull_request", "status": "success"}
    job = {"id": 20, "run_id": 12, "repo_id": 7, "name": "check", "status": "success", "attempt": 1}
    observation = {"pr": {"number": 3, "state": "open", "draft": False, "merged": False,
                          "user": {"login": "author"}, "head": {"sha": HEAD},
                          "base": {"ref": "main", "repo": {"full_name": "qos/qos"}}},
                   "base": {"commit": {"id": BASE}}, "reviews": [review],
                   "runs": [{"run": run, "jobs": [job]}]}
    return policy, observation


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.policy, self.observation = fixture()

    def errors(self):
        return AUDIT.observation_errors(self.observation, "qos/qos", 3, "main", self.policy)

    def test_exact_evidence_passes(self):
        self.assertEqual([], AUDIT.policy_errors(self.policy))
        self.assertEqual([], self.errors())

    def test_changed_stale_dismissed_self_or_untrusted_review_fails(self):
        for field, value in (("commit_id", BASE), ("dismissed", True), ("official", False),
                             ("user", {"login": "author"}), ("user", {"login": "stranger"})):
            with self.subTest(field=field, value=value):
                self.policy, self.observation = fixture()
                self.observation["reviews"][0][field] = value
                self.assertTrue(self.errors())

    def test_outstanding_change_request_blocks_even_with_approval(self):
        self.observation["reviews"].append({**self.observation["reviews"][0], "state": "REQUEST_CHANGES"})
        self.assertTrue(self.errors())

    def test_status_forgery_cannot_mask_run_or_job_failure(self):
        for state in ("failure", "cancelled", "waiting", "skipped", None):
            for subject in ("run", "job"):
                with self.subTest(state=state, subject=subject):
                    self.policy, self.observation = fixture()
                    actual = self.observation["runs"][0]
                    target = actual["run"] if subject == "run" else actual["jobs"][0]
                    target["status"] = state
                    self.observation["commit_status"] = "success"
                    self.assertTrue(self.errors())

    def test_exact_job_set_and_attempt_required(self):
        for change in ("absent", "duplicate", "unexpected", "foreign", "attempt"):
            with self.subTest(change=change):
                self.policy, self.observation = fixture()
                jobs = self.observation["runs"][0]["jobs"]
                if change == "absent": jobs.clear()
                if change == "duplicate": jobs.append(copy.deepcopy(jobs[0]))
                if change == "unexpected": jobs[0]["name"] = "other"
                if change == "foreign": jobs[0]["repo_id"] = 88
                if change == "attempt": jobs[0].pop("attempt")
                self.assertTrue(self.errors())

    def test_workflow_identity_and_event_are_bound(self):
        for key in ("id", "commit_sha", "workflow_id", "event", "repository"):
            with self.subTest(key=key):
                self.policy, self.observation = fixture()
                self.observation["runs"][0]["run"][key] = {} if key == "repository" else "wrong"
                self.assertTrue(self.errors())

    def test_draft_closed_or_merged_is_not_ready(self):
        for key, value in (("draft", True), ("state", "closed"), ("merged", True)):
            self.policy, self.observation = fixture()
            self.observation["pr"][key] = value
            self.assertTrue(self.errors())

    def test_policy_requires_explicit_workflow_protection_and_job_set(self):
        for key, value in (("protected_paths", ["tools"]), ("trusted_revision", "main"),
                           ("runs", []), ("reviewers", []), ("protected_paths", [":(glob)*"])):
            with self.subTest(key=key):
                policy, _ = fixture()
                policy[key] = value
                self.assertTrue(AUDIT.policy_errors(policy))

    def test_retry_or_review_change_changes_fingerprint(self):
        before = AUDIT.fingerprint(self.observation)
        self.observation["runs"][0]["jobs"][0]["attempt"] = 2
        self.assertNotEqual(before, AUDIT.fingerprint(self.observation))

    def test_review_pagination_is_complete(self):
        seen = []
        def read(host, repo, path):
            seen.append(path)
            if path == "/pulls/3": return self.observation["pr"]
            if path == "/branches/main": return self.observation["base"]
            if "/reviews?" in path: return self.observation["reviews"] * (50 if "page=1" in path else 1)
            if path.endswith("/jobs"): return self.observation["runs"][0]["jobs"]
            return self.observation["runs"][0]["run"]
        result = AUDIT.collect("host", "qos/qos", 3, "main", self.policy, read)
        self.assertEqual(51, len(result["reviews"]))
        self.assertTrue(any("page=2" in p for p in seen))

    def test_malformed_api_response_still_writes_rejected_receipt(self):
        for malformed in (None, [], {"head": None}):
            with self.subTest(malformed=malformed), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                policy_path, receipt_path = root / "policy.json", root / "receipt.json"
                policy_path.write_text(json.dumps(self.policy))
                args = ["audit", "--repo", "qos/qos", "--pr", "3", "--git-root", directory,
                        "--policy", str(policy_path), "--out", str(receipt_path)]
                # First PR read malformed; other reads retain realistic fixture
                # types so rejection is exercised in the production evaluator.
                responses = [malformed, self.observation["base"], self.observation["reviews"],
                             self.observation["runs"][0]["run"], self.observation["runs"][0]["jobs"]]
                with mock.patch("sys.argv", args), mock.patch.object(AUDIT, "api", side_effect=responses), mock.patch("builtins.print"):
                    self.assertEqual(2, AUDIT.main())
                receipt = json.loads(receipt_path.read_text())
                self.assertFalse(receipt["observed_ready"])
                self.assertFalse(receipt["merge_authorized"])
                self.assertTrue(any("incomplete observation" in e for e in receipt["errors"]))


class GitProvenanceTests(unittest.TestCase):
    def test_real_git_ancestry_and_helper_addition_modification_deletion(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def git(*args):
                return subprocess.check_output(["git", "-C", directory, *args], stderr=subprocess.DEVNULL).decode().strip()
            git("init", "-q")
            git("config", "user.name", "fixture")
            git("config", "user.email", "fixture@example.invalid")
            (root / "tools").mkdir()
            (root / "tools/check.py").write_text("trusted\n")
            (root / "app.txt").write_text("first\n")
            git("add", "tools/check.py", "app.txt")
            git("commit", "-qm", "base")
            base = git("rev-parse", "HEAD")
            policy = {"trusted_revision": base, "protected_paths": ["tools"]}
            (root / "app.txt").write_text("candidate\n")
            git("add", "app.txt"); git("commit", "-qm", "candidate")
            head = git("rev-parse", "HEAD")
            self.assertEqual([], AUDIT.provenance_errors(root, base, head, policy))
            for change in ("add", "modify", "delete"):
                if change == "add": (root / "tools/new.py").write_text("new\n")
                if change == "modify": (root / "tools/check.py").write_text("weakened\n")
                if change == "delete": (root / "tools/check.py").unlink()
                git("add", "tools"); git("commit", "-qm", change)
                self.assertTrue(AUDIT.provenance_errors(root, base, git("rev-parse", "HEAD"), policy))
            # A newer integration base cannot be certified by an older candidate.
            self.assertTrue(AUDIT.provenance_errors(root, git("rev-parse", "HEAD"), head, policy))


if __name__ == "__main__":
    unittest.main()
