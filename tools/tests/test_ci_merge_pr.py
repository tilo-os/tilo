import importlib.util
import errno
import hashlib
import io
import json
from pathlib import Path
import shutil
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
    """A work tree with an `origin` bare remote, both holding one base commit.

    Building it costs a dozen git processes, so the first one is built once as a
    template and every Repo is an independent copy of that template whose
    `origin` is re-pointed at its own copied remote: no state is shared.
    """

    _template = None

    @classmethod
    def build_template(cls):
        if cls._template is None:
            cls._template_dir = tempfile.TemporaryDirectory()
            home = Path(cls._template_dir.name)
            root = home / "work"
            remote = home / "origin.git"
            root.mkdir()
            command(root, "init", "-q")
            subprocess.run(["git", "init", "--bare", "-q", str(remote)], check=True)
            command(root, "remote", "add", "origin", str(remote))
            command(root, "config", "user.email", "test@example.invalid")
            command(root, "config", "user.name", "Test")
            path = root / "apps/calculator/src/main.rs"
            path.parent.mkdir(parents=True)
            path.write_text("fn main() {}\n")
            (path.parents[1] / "Cargo.toml").write_text("[package]\nname='calculator'\nversion='0.0.0'\n")
            command(root, "add", "apps/calculator")
            command(root, "commit", "-qm", "base")
            command(root, "branch", "-M", "main")
            base = command(root, "rev-parse", "HEAD")
            command(root, "push", "-qu", "origin", "main")
            cls._template = (home, base)
        return cls._template

    def __init__(self):
        template, self.base = self.build_template()
        self.temp = tempfile.TemporaryDirectory()
        home = Path(self.temp.name)
        self.root = home / "work"
        self.remote = home / "origin.git"
        shutil.copytree(template / "work", self.root, symlinks=True)
        shutil.copytree(template / "origin.git", self.remote, symlinks=True)
        command(self.root, "remote", "set-url", "origin", str(self.remote))
        assert command(self.root, "rev-parse", "HEAD") == self.base
        assert command(self.root, "status", "--porcelain") == ""

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


def branch_summary(commit, *, can_merge=True):
    return {
        "commit": {"id": commit},
        "protected": True,
        "enable_status_check": True,
        "status_check_contexts": sorted(merge_pr.REQUIRED_CONTEXTS),
        "user_can_merge": can_merge,
        "user_can_push": False,
        "required_approvals": 1,
    }


def protection_rule():
    return {
        "enable_push": False,
        "apply_to_admins": True,
        "enable_merge_whitelist": False,
        "merge_whitelist_usernames": [],
        "merge_whitelist_teams": [],
        "enable_status_check": True,
        "status_check_contexts": sorted(merge_pr.REQUIRED_CONTEXTS),
        "required_approvals": 1,
        "enable_approvals_whitelist": True,
        "approvals_whitelist_username": sorted(merge_pr.REVIEWERS),
        "approvals_whitelist_teams": [],
        "block_on_rejected_reviews": True,
        "block_on_official_review_requests": True,
        "block_on_outdated_branch": True,
        "dismiss_stale_approvals": True,
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
            if self.actor != merge_pr.INTEGRATOR:
                raise PermissionError("account is outside the sole merge whitelist")
            self.posts.append(body)
            command(self.root, "push", "-q", "--force", "origin",
                    f"{self.head}:refs/heads/main")
            self.merged = True
            return 200, {}
        path = path.split("&page=", 1)[0].split("?limit=", 1)[0]
        if path == "":
            return 200, {"full_name": merge_pr.REPO, "private": True}
        if path == "/branches/main":
            commit = self.main_after_merge if self.merged else self.base
            return 200, branch_summary(commit, can_merge=self.actor == merge_pr.INTEGRATOR)
        if path == "/branch_protections/main":
            return 200, protection_rule()
        if path == "/pulls/7":
            return 200, {
                "state": "closed" if self.merged else "open", "merged": self.merged,
                "draft": False, "head": {"sha": self.head}, "base": {"ref": "main"},
                "user": {"login": "author"},
                "merge_commit_sha": self.head if self.merged else None,
                "merged_by": {"login": self.actor} if self.merged else None,
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
    def test_routine_filter_accepts_only_current_app_rust_source_changes(self):
        with self.subTest(phase='accepts only app rust source additions and modifications'):
            repo = Repo()
            try:
                head = repo.candidate()
                self.assertEqual(merge_pr.routine_errors(repo.root, repo.base, head), [])
            finally:
                repo.close()
        with self.subTest(phase='rejects policy paths and deletions'):
            for path, remove in [("Cargo.toml", False), ("apps/calculator/src/main.rs", True)]:
                with self.subTest(path=path, remove=remove):
                    repo = Repo()
                    try:
                        head = repo.candidate(path, "[workspace]\n", remove)
                        self.assertTrue(merge_pr.routine_errors(repo.root, repo.base, head))
                    finally:
                        repo.close()
        with self.subTest(phase='rejects source for an unknown app'):
            repo = Repo()
            try:
                head = repo.candidate("apps/not-a-package/src/main.rs")
                self.assertIn("source is outside an existing app package: apps/not-a-package/src/main.rs",
                              merge_pr.routine_errors(repo.root, repo.base, head))
            finally:
                repo.close()
        with self.subTest(phase='rejects a stale candidate'):
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
        with self.subTest(phase='prefilter raises when the trusted checkout moves or is dirty'):
            repo = Repo()
            try:
                head = repo.candidate()
                with self.assertRaisesRegex(ValueError, "moved"):
                    merge_pr.routine_prefilter_errors(repo.root, "f" * 40, head)
                (repo.root / "untracked.txt").write_text("dirty\n")
                with self.assertRaisesRegex(ValueError, "dirty"):
                    merge_pr.routine_prefilter_errors(repo.root, repo.base, head)
            finally:
                repo.close()


class AdmissionTests(unittest.TestCase):
    def setUp(self):
        # Merged tests call setUp() again per phase: every phase gets its own repo.
        self.repo = Repo()
        self.addCleanup(self.repo.close)
        self.head = self.repo.candidate()

    def test_observation_requires_exact_head_approval_and_fail_closed_protection(self):
        with self.subTest(phase='observes exact head checks and approval'):
            self.setUp()
            result = merge_pr.observe(FakeApi(self.repo.root, self.head), 7, self.repo.root)
            self.assertEqual(result["errors"], [])
        with self.subTest(phase='stale approval and wrong check head fail'):
            self.setUp()
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
        with self.subTest(phase='every exposed branch protection field fails closed'):
            self.setUp()
            changes = {
                "protected": False,
                "enable_status_check": False,
                "status_check_contexts": ["wrong"],
                "required_approvals": 0,
                "user_can_merge": False,
                "user_can_push": True,
            }
            for field, value in changes.items():
                with self.subTest(field=field):
                    api = FakeApi(self.repo.root, self.head)
                    original = api.__call__

                    def changed(path, method="GET", body=None, *, field=field, value=value):
                        status, response = original(path, method, body)
                        if path == "/branches/main":
                            response[field] = value
                        return status, response

                    result = merge_pr.observe(changed, 7, self.repo.root)
                    if field == "required_approvals":
                        expected = "branch protection requires no independent approval"
                    else:
                        expected = "branch protection mismatch: " + field
                    self.assertIn(expected, result["errors"])
        with self.subTest(phase='extra human merge authority fails closed'):
            self.setUp()
            api = FakeApi(self.repo.root, self.head)
            original = api.__call__

            def changed(path, method="GET", body=None):
                status, value = original(path, method, body)
                if path == "/branch_protections/main":
                    value["merge_whitelist_usernames"] = [merge_pr.INTEGRATOR, "admin"]
                return status, value

            result = merge_pr.observe(changed, 7, self.repo.root)
            self.assertIn(
                "branch protection rule mismatch: merge_whitelist_usernames",
                result["errors"],
            )
        with self.subTest(phase='every live rule field fails closed'):
            self.setUp()
            baseline = protection_rule()
            changes = {
                key: (not value if isinstance(value, bool) else ["wrong"])
                for key, value in baseline.items()
                if key != "required_approvals"
            }
            changes["required_approvals"] = 0
            for field, value in changes.items():
                with self.subTest(field=field):
                    rule = protection_rule()
                    rule[field] = value
                    errors = merge_pr.protection_rule_errors(rule)
                    expected = ("branch protection rule requires no independent approval"
                                if field == "required_approvals"
                                else "branch protection rule mismatch: " + field)
                    self.assertIn(expected, errors)
        with self.subTest(phase='wrong account cannot pass effective branch permission'):
            self.setUp()
            with tempfile.TemporaryDirectory() as directory:
                journal = Path(directory) / "intent.json"
                api = FakeApi(self.repo.root, self.head, journal, actor="author")
                self.assertEqual(merge_pr.merge(api, 7, self.repo.root, journal, execute=True), 2)
                self.assertEqual(api.posts, [])
                receipt = json.loads(journal.read_text())
                self.assertEqual(receipt["state"], "rejected")
                self.assertIn("branch protection mismatch: user_can_merge", receipt["errors"])

    def test_merge_posts_once_after_durable_intent_and_verifies_readback(self):
        with self.subTest(phase='merge persists intent and pins head then verifies readback'):
            self.setUp()
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
        with self.subTest(phase='dry run never posts'):
            self.setUp()
            with tempfile.TemporaryDirectory() as directory:
                journal = Path(directory) / "intent.json"
                api = FakeApi(self.repo.root, self.head, journal)
                self.assertEqual(merge_pr.merge(api, 7, self.repo.root, journal), 0)
                self.assertEqual(api.posts, [])
                self.assertEqual(json.loads(journal.read_text())["state"], "dry_run_ready")
        with self.subTest(phase='reconciles an existing intent without another post'):
            self.setUp()
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

    def test_reconcile_accepts_only_main_containing_our_merged_head(self):
        with self.subTest(phase='reconcile accepts a later main that contains the merge'):
            self.setUp()
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
        with self.subTest(phase='reconcile rejects main without the merged head'):
            self.setUp()
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
        with self.subTest(phase='reconcile rejects a merge attributed to another account'):
            self.setUp()
            with tempfile.TemporaryDirectory() as directory:
                journal = Path(directory) / "intent.json"
                journal.write_text(json.dumps({"repo": merge_pr.REPO, "pr": 7,
                                               "head": self.head,
                                               "state": "merge_intent_do_not_retry"}))
                self.repo.publish(self.head)
                api = FakeApi(self.repo.root, self.head, journal, actor="author")
                api.merged = True
                self.assertEqual(merge_pr.reconcile(api, journal, self.repo.root), 3)
                receipt = json.loads(journal.read_text())
                self.assertEqual(receipt["state"], "not_verified_do_not_retry")
                self.assertEqual(receipt["readback"]["merged_by"], "author")
                self.assertTrue(receipt["readback"]["head_contained_in_main"])
        with self.subTest(phase='reconcile rejects api and fetched main mismatch'):
            self.setUp()
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
        with self.subTest(phase='reconcile rejects fetch failure'):
            self.setUp()
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
    def test_journal_save_and_directory_fsync_are_durable_and_portable(self):
        with self.subTest(phase='save syncs parent after create and replace'):
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "journal.json"
                with mock.patch.object(merge_pr, "fsync_parent") as sync:
                    merge_pr.save(path, {"state": "one"}, exclusive=True)
                    merge_pr.save(path, {"state": "two"})
                self.assertEqual(sync.call_args_list, [mock.call(path), mock.call(path)])
        with self.subTest(phase='directory fsync is portable when directory open is unsupported'):
            unsupported = OSError(errno.ENOTSUP, "unsupported")
            with mock.patch.object(merge_pr.os, "open", side_effect=unsupported):
                merge_pr.fsync_parent(Path("journal.json"))
        with self.subTest(phase='directory fsync does not hide io errors'):
            with mock.patch.object(merge_pr.os, "open", side_effect=OSError(errno.EIO, "failed")):
                with self.assertRaises(OSError):
                    merge_pr.fsync_parent(Path("journal.json"))


class CredentialBindingTests(unittest.TestCase):
    TOKEN = "repository-scoped-test-token"

    def fingerprint(self):
        return hashlib.sha256(self.TOKEN.encode()).hexdigest()

    def test_only_the_fingerprint_bound_token_reaches_the_api(self):
        with self.subTest(phase='accepts only the bound token with constant time comparison'):
            environ = {"FORGEJO_TOKEN": self.TOKEN,
                       "FORGEJO_TOKEN_SHA256": self.fingerprint()}
            with mock.patch.object(merge_pr.hmac, "compare_digest",
                                   wraps=merge_pr.hmac.compare_digest) as compare:
                self.assertEqual(merge_pr.bound_token(environ), self.TOKEN)
            compare.assert_called_once_with(self.fingerprint(), self.fingerprint())
        with self.subTest(phase='missing malformed and wrong fingerprints fail closed'):
            for fingerprint in (None, "A" * 64, "0" * 64):
                with self.subTest(fingerprint=fingerprint):
                    environ = {"FORGEJO_TOKEN": self.TOKEN}
                    if fingerprint is not None:
                        environ["FORGEJO_TOKEN_SHA256"] = fingerprint
                    self.assertEqual(merge_pr.bound_token(environ), "")
        with self.subTest(phase='worker entrypoint rejects missing or wrong binding before api construction'):
            for fingerprint in (None, "0" * 64):
                with self.subTest(fingerprint=fingerprint), tempfile.TemporaryDirectory() as directory, \
                        mock.patch.object(merge_pr, "Api", side_effect=AssertionError("API called")), \
                        mock.patch("sys.stderr", new=io.StringIO()):
                    environ = {"FORGEJO_TOKEN": self.TOKEN}
                    if fingerprint is not None:
                        environ["FORGEJO_TOKEN_SHA256"] = fingerprint
                    with self.assertRaises(SystemExit) as raised:
                        merge_pr.main([
                            "--pr", "7", "--git-root", str(ROOT),
                            "--journal", str(Path(directory) / "intent.json"), "--execute",
                        ], environ)
                    self.assertEqual(raised.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
