import importlib.util
import errno
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("ci_merge_pr", ROOT / "tools/ci/merge_pr.py")
merge_pr = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(merge_pr)


def command(root, *args):
    return subprocess.run(["git", "-C", str(root), *args], check=True,
                          capture_output=True, text=True).stdout.strip()


class Repo:
    def __init__(self):
        self.temp = tempfile.TemporaryDirectory()
        home = Path(self.temp.name)
        self.root = home / "work"
        self.remote = home / "origin.git"
        self.root.mkdir()
        command(self.root, "init", "-q")
        subprocess.run(["git", "init", "--bare", "-q", str(self.remote)], check=True)
        command(self.root, "remote", "add", "origin", str(self.remote))
        command(self.root, "config", "user.email", "test@example.invalid")
        command(self.root, "config", "user.name", "Test")
        path = self.root / "apps/calculator/src/main.rs"
        path.parent.mkdir(parents=True)
        path.write_text("fn main() {}\n")
        (path.parents[1] / "Cargo.toml").write_text("[package]\nname='calculator'\nversion='0.0.0'\n")
        command(self.root, "add", "apps/calculator")
        command(self.root, "commit", "-qm", "base")
        command(self.root, "branch", "-M", "main")
        self.base = command(self.root, "rev-parse", "HEAD")
        command(self.root, "push", "-qu", "origin", "main")

    def candidate(self, path="apps/calculator/src/main.rs", content="fn main() { ok(); }\n",
                  remove=False):
        target = self.root / path
        if remove:
            target.unlink()
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(content)
        command(self.root, "add", "-A", path)
        command(self.root, "commit", "-qm", "candidate")
        head = command(self.root, "rev-parse", "HEAD")
        command(self.root, "checkout", "-q", self.base)
        return head

    def publish(self, commit):
        command(self.root, "push", "-q", "--force", "origin", f"{commit}:refs/heads/main")

    def advance_remote(self):
        publisher = Path(self.temp.name) / "publisher"
        subprocess.run(["git", "clone", "-q", "--branch", "main",
                        str(self.remote), str(publisher)], check=True)
        command(publisher, "config", "user.email", "test@example.invalid")
        command(publisher, "config", "user.name", "Test")
        (publisher / "later.txt").write_text("later\n")
        command(publisher, "add", "later.txt")
        command(publisher, "commit", "-qm", "later main")
        later = command(publisher, "rev-parse", "HEAD")
        command(publisher, "push", "-q", "origin", "main")
        return later

    def close(self):
        self.temp.cleanup()


def protection():
    return {
        "enable_push": False, "apply_to_admins": True,
        "enable_merge_whitelist": True,
        "merge_whitelist_usernames": [merge_pr.INTEGRATOR],
        "merge_whitelist_teams": [],
        "enable_status_check": True,
        "status_check_contexts": sorted(merge_pr.REQUIRED_CONTEXTS),
        "enable_approvals_whitelist": True,
        "approvals_whitelist_username": sorted(merge_pr.REVIEWERS),
        "approvals_whitelist_teams": [],
        "block_on_outdated_branch": True, "block_on_rejected_reviews": True,
        "block_on_official_review_requests": True,
        "dismiss_stale_approvals": True, "ignore_stale_approvals": False,
        "required_approvals": 1,
    }


class FakeApi:
    def __init__(self, root, head, journal=None, actor=merge_pr.INTEGRATOR):
        self.root, self.head, self.base = root, head, command(root, "rev-parse", "HEAD")
        self.journal = journal
        self.actor = actor
        self.merged = False
        self.main_after_merge = head
        self.posts = []

    def __call__(self, path, method="GET", body=None):
        if method == "POST":
            state = json.loads(self.journal.read_text())["state"] if self.journal else None
            if state != "merge_intent_do_not_retry":
                raise AssertionError("merge intent was not durable before POST")
            self.posts.append(body)
            command(self.root, "push", "-q", "--force", "origin",
                    f"{self.head}:refs/heads/main")
            self.merged = True
            return 200, {}
        path = path.split("&page=", 1)[0].split("?limit=", 1)[0]
        if path == "/user":
            return 200, {"login": self.actor}
        if path == "":
            return 200, {"full_name": merge_pr.REPO, "private": True}
        if path == "/branches/main":
            return 200, {"commit": {"id": self.main_after_merge if self.merged else self.base}}
        if path == "/branch_protections/main":
            return 200, protection()
        if path == "/pulls/7":
            return 200, {
                "state": "closed" if self.merged else "open", "merged": self.merged,
                "draft": False, "head": {"sha": self.head}, "base": {"ref": "main"},
                "user": {"login": "author"},
                "merge_commit_sha": self.head if self.merged else None,
                "merged_by": {"login": merge_pr.INTEGRATOR} if self.merged else None,
            }
        if path == "/pulls/7/reviews":
            return 200, [{"id": 1, "state": "APPROVED", "official": True,
                          "dismissed": False, "commit_id": self.head,
                          "user": {"login": "claude"}}]
        if path.startswith("/actions/runs?head_sha="):
            return 200, {"total_count": 2, "workflow_runs": [
                {"id": 11, "workflow_id": "check.yml", "event": "pull_request",
                 "commit_sha": self.head, "status": "success"},
                {"id": 12, "workflow_id": "lint.yml", "event": "pull_request",
                 "commit_sha": self.head, "status": "success"},
            ]}
        if path == "/actions/runs/11/jobs":
            return 200, [{"id": 21, "name": "checks", "status": "success"},
                         {"id": 22, "name": "report-check-result", "status": "skipped"}]
        if path == "/actions/runs/12/jobs":
            return 200, [{"id": 23, "name": "quality", "status": "success"}]
        raise AssertionError(path)


class RoutineTests(unittest.TestCase):
    def test_accepts_only_app_rust_source_additions_and_modifications(self):
        repo = Repo()
        try:
            head = repo.candidate()
            self.assertEqual(merge_pr.routine_errors(repo.root, repo.base, head), [])
        finally:
            repo.close()

    def test_rejects_policy_paths_and_deletions(self):
        for path, remove in [("Cargo.toml", False), ("apps/calculator/src/main.rs", True)]:
            with self.subTest(path=path, remove=remove):
                repo = Repo()
                try:
                    head = repo.candidate(path, "[workspace]\n", remove)
                    self.assertTrue(merge_pr.routine_errors(repo.root, repo.base, head))
                finally:
                    repo.close()

    def test_rejects_source_for_an_unknown_app(self):
        repo = Repo()
        try:
            head = repo.candidate("apps/not-a-package/src/main.rs")
            self.assertIn("source is outside an existing app package: apps/not-a-package/src/main.rs",
                          merge_pr.routine_errors(repo.root, repo.base, head))
        finally:
            repo.close()

    def test_rejects_a_stale_candidate(self):
        repo = Repo()
        try:
            head = repo.candidate()
            (repo.root / "README.md").write_text("new base\n")
            command(repo.root, "add", "README.md")
            command(repo.root, "commit", "-qm", "advance main")
            new_base = command(repo.root, "rev-parse", "HEAD")
            self.assertIn("candidate does not contain current main",
                          merge_pr.routine_errors(repo.root, new_base, head))
        finally:
            repo.close()


class AdmissionTests(unittest.TestCase):
    def setUp(self):
        self.repo = Repo()
        self.head = self.repo.candidate()

    def tearDown(self):
        self.repo.close()

    def test_observes_exact_head_checks_and_approval(self):
        result = merge_pr.observe(FakeApi(self.repo.root, self.head), 7, self.repo.root)
        self.assertEqual(result["errors"], [])

    def test_stale_approval_and_wrong_check_head_fail(self):
        api = FakeApi(self.repo.root, self.head)
        original = api.__call__

        def changed(path, method="GET", body=None):
            status, value = original(path, method, body)
            if path.startswith("/pulls/7/reviews"):
                value[0]["commit_id"] = "0" * 40
            if path.startswith("/actions/runs?head_sha="):
                value["workflow_runs"][0]["commit_sha"] = "1" * 40
            return status, value

        result = merge_pr.observe(changed, 7, self.repo.root)
        self.assertIn("missing allowed independent approval on exact head", result["errors"])
        self.assertIn("missing exact-head check.yml run", result["errors"])

    def test_unqualified_protection_fails_closed(self):
        api = FakeApi(self.repo.root, self.head)
        original = api.__call__

        def changed(path, method="GET", body=None):
            status, value = original(path, method, body)
            if path == "/branch_protections/main":
                value["required_approvals"] = 0
                value["merge_whitelist_usernames"] = [merge_pr.INTEGRATOR, "author"]
            return status, value

        result = merge_pr.observe(changed, 7, self.repo.root)
        self.assertIn("branch protection requires no independent approval", result["errors"])
        self.assertIn("branch protection mismatch: merge_whitelist_usernames", result["errors"])

    def test_wrong_token_actor_rejects_without_posting(self):
        with tempfile.TemporaryDirectory() as directory:
            journal = Path(directory) / "intent.json"
            api = FakeApi(self.repo.root, self.head, journal, actor="author")
            self.assertEqual(merge_pr.merge(api, 7, self.repo.root, journal, execute=True), 2)
            self.assertEqual(api.posts, [])
            self.assertIn("token does not belong to the integration service",
                          json.loads(journal.read_text())["errors"])

    def test_merge_persists_intent_and_pins_head_then_verifies_readback(self):
        with tempfile.TemporaryDirectory() as directory:
            journal = Path(directory) / "intent.json"
            api = FakeApi(self.repo.root, self.head, journal)
            self.assertEqual(merge_pr.merge(api, 7, self.repo.root, journal, execute=True), 0)
            self.assertEqual(api.posts, [{"Do": "fast-forward-only",
                                          "head_commit_id": self.head,
                                          "force_merge": False,
                                          "merge_when_checks_succeed": False}])
            receipt = json.loads(journal.read_text())
            self.assertEqual(receipt["state"], "merged_verified")
            self.assertEqual(receipt["readback"]["merged_by"], merge_pr.INTEGRATOR)

    def test_dry_run_never_posts(self):
        with tempfile.TemporaryDirectory() as directory:
            journal = Path(directory) / "intent.json"
            api = FakeApi(self.repo.root, self.head, journal)
            self.assertEqual(merge_pr.merge(api, 7, self.repo.root, journal), 0)
            self.assertEqual(api.posts, [])
            self.assertEqual(json.loads(journal.read_text())["state"], "dry_run_ready")

    def test_reconciles_an_existing_intent_without_another_post(self):
        with tempfile.TemporaryDirectory() as directory:
            journal = Path(directory) / "intent.json"
            journal.write_text(json.dumps({"repo": merge_pr.REPO, "pr": 7,
                                           "head": self.head,
                                           "state": "merge_intent_do_not_retry"}))
            api = FakeApi(self.repo.root, self.head, journal)
            self.repo.publish(self.head)
            api.merged = True
            self.assertEqual(merge_pr.reconcile(api, journal, self.repo.root), 0)
            self.assertEqual(api.posts, [])
            self.assertEqual(json.loads(journal.read_text())["state"], "merged_verified")

    def test_reconcile_accepts_a_later_main_that_contains_the_merge(self):
        with tempfile.TemporaryDirectory() as directory:
            journal = Path(directory) / "intent.json"
            journal.write_text(json.dumps({"repo": merge_pr.REPO, "pr": 7,
                                           "head": self.head,
                                           "state": "merge_intent_do_not_retry"}))
            self.repo.publish(self.head)
            later = self.repo.advance_remote()
            missing = subprocess.run(["git", "-C", str(self.repo.root), "cat-file", "-e", later])
            self.assertNotEqual(missing.returncode, 0)
            api = FakeApi(self.repo.root, self.head, journal)
            api.merged = True
            api.main_after_merge = later
            self.assertEqual(merge_pr.reconcile(api, journal, self.repo.root), 0)
            receipt = json.loads(journal.read_text())
            self.assertEqual(receipt["readback"]["main"], later)
            self.assertEqual(receipt["readback"]["fetched_main"], later)
            self.assertTrue(receipt["readback"]["head_contained_in_main"])
            self.assertEqual(command(self.repo.root, "rev-parse", merge_pr.FETCHED_MAIN_REF), later)

    def test_reconcile_rejects_main_without_the_merged_head(self):
        with tempfile.TemporaryDirectory() as directory:
            journal = Path(directory) / "intent.json"
            journal.write_text(json.dumps({"repo": merge_pr.REPO, "pr": 7,
                                           "head": self.head,
                                           "state": "merge_intent_do_not_retry"}))
            api = FakeApi(self.repo.root, self.head, journal)
            api.merged = True
            api.main_after_merge = self.repo.base
            self.assertEqual(merge_pr.reconcile(api, journal, self.repo.root), 3)
            receipt = json.loads(journal.read_text())
            self.assertEqual(receipt["state"], "not_verified_do_not_retry")
            self.assertFalse(receipt["readback"]["head_contained_in_main"])

    def test_reconcile_rejects_api_and_fetched_main_mismatch(self):
        with tempfile.TemporaryDirectory() as directory:
            journal = Path(directory) / "intent.json"
            journal.write_text(json.dumps({"repo": merge_pr.REPO, "pr": 7,
                                           "head": self.head,
                                           "state": "merge_intent_do_not_retry"}))
            api = FakeApi(self.repo.root, self.head, journal)
            api.merged = True
            self.assertEqual(merge_pr.reconcile(api, journal, self.repo.root), 3)
            receipt = json.loads(journal.read_text())
            self.assertEqual(receipt["readback"]["main"], self.head)
            self.assertEqual(receipt["readback"]["fetched_main"], self.repo.base)
            self.assertFalse(receipt["readback"]["head_contained_in_main"])

    def test_reconcile_rejects_fetch_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            journal = Path(directory) / "intent.json"
            journal.write_text(json.dumps({"repo": merge_pr.REPO, "pr": 7,
                                           "head": self.head,
                                           "state": "merge_intent_do_not_retry"}))
            command(self.repo.root, "remote", "remove", "origin")
            api = FakeApi(self.repo.root, self.head, journal)
            api.merged = True
            self.assertEqual(merge_pr.reconcile(api, journal, self.repo.root), 3)
            receipt = json.loads(journal.read_text())
            self.assertEqual(receipt["readback"]["fetched_main"], "")
            self.assertFalse(receipt["readback"]["head_contained_in_main"])


class JournalDurabilityTests(unittest.TestCase):
    def test_save_syncs_parent_after_create_and_replace(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "journal.json"
            with mock.patch.object(merge_pr, "fsync_parent") as sync:
                merge_pr.save(path, {"state": "one"}, exclusive=True)
                merge_pr.save(path, {"state": "two"})
            self.assertEqual(sync.call_args_list, [mock.call(path), mock.call(path)])

    def test_directory_fsync_is_portable_when_directory_open_is_unsupported(self):
        unsupported = OSError(errno.ENOTSUP, "unsupported")
        with mock.patch.object(merge_pr.os, "open", side_effect=unsupported):
            merge_pr.fsync_parent(Path("journal.json"))

    def test_directory_fsync_does_not_hide_io_errors(self):
        with mock.patch.object(merge_pr.os, "open", side_effect=OSError(errno.EIO, "failed")):
            with self.assertRaises(OSError):
                merge_pr.fsync_parent(Path("journal.json"))


if __name__ == "__main__":
    unittest.main()
