import importlib.util
import hashlib
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
BASE = subprocess.check_output(["git", "-C", str(ROOT), "rev-parse", "HEAD"], text=True).strip()
SPEC = importlib.util.spec_from_file_location(
    "ci_merge_dispatch", ROOT / "tools/ci/dispatch_merge.py"
)
dispatch_merge = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(dispatch_merge)
merge_pr = dispatch_merge.merge_pr


HEAD_7 = "7" * 40
HEAD_9 = "9" * 40


class ListApi:
    def __init__(self, rows, base=BASE):
        self.rows = rows
        self.base = base
        self.calls = []

    def __call__(self, path, method="GET", body=None):
        self.calls.append((path, method, body))
        if path.startswith("/pulls?state=open"):
            return 200, self.rows
        if path == "/branches/main":
            return 200, {"commit": {"id": self.base}}
        raise AssertionError(path)


def pr(number, head):
    return {"id": number + 100, "number": number, "head": {"sha": head}}


def command(root, *args):
    return subprocess.run(["git", "-C", str(root), *args], check=True,
                          capture_output=True, text=True).stdout.strip()


class MainRepo:
    def __init__(self):
        self.temp = tempfile.TemporaryDirectory()
        home = Path(self.temp.name)
        self.remote = home / "origin.git"
        self.root = home / "work"
        subprocess.run(["git", "init", "--bare", "-q", str(self.remote)], check=True)
        subprocess.run(["git", "clone", "-q", str(self.remote), str(self.root)], check=True)
        command(self.root, "config", "user.email", "test@example.invalid")
        command(self.root, "config", "user.name", "Test")
        (self.root / "tracked.txt").write_text("one\n")
        command(self.root, "add", "tracked.txt")
        command(self.root, "commit", "-qm", "base")
        command(self.root, "branch", "-M", "main")
        command(self.root, "push", "-qu", "origin", "main")

    def advance(self):
        publisher = Path(self.temp.name) / "publisher"
        subprocess.run(["git", "clone", "-q", "--branch", "main",
                        str(self.remote), str(publisher)], check=True)
        command(publisher, "config", "user.email", "test@example.invalid")
        command(publisher, "config", "user.name", "Test")
        (publisher / "tracked.txt").write_text("two\n")
        command(publisher, "add", "tracked.txt")
        command(publisher, "commit", "-qm", "advance")
        head = command(publisher, "rev-parse", "HEAD")
        command(publisher, "push", "-q", "origin", "main")
        return head

    def close(self):
        self.temp.cleanup()


class DispatchTests(unittest.TestCase):
    def setUp(self):
        # Merged tests reset this per phase with doCleanups() + setUp().
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.state = Path(self.temp.name)
        self.root = ROOT

    def write_journal(self, name, state, head=HEAD_7, number=7):
        path = self.state / name
        path.write_text(json.dumps({"schema": 1, "repo": merge_pr.REPO, "state": state,
                                    "head": head, "pr": number}))
        return path

    def dispatch(self, api, **kwargs):
        kwargs.setdefault("refresh", lambda *_: True)
        kwargs.setdefault("classify", lambda *_: [])
        return dispatch_merge.dispatch(api, self.root, self.state, **kwargs)

    def test_journal_and_lease_prevent_any_repeated_or_concurrent_merge(self):
        with self.subTest(phase='exclusive process lease skips a concurrent dispatch'):
            self.doCleanups()
            self.setUp()
            api = ListApi([])
            with dispatch_merge.ProcessLease(self.state / "dispatcher.lock"):
                result = self.dispatch(api)
            self.assertEqual(result, 0)
            self.assertEqual(api.calls, [])
        with self.subTest(phase='reconciles one unresolved intent and stops before scanning'):
            self.doCleanups()
            self.setUp()
            path = self.write_journal("merge-7.json", "merge_intent_do_not_retry")
            api = ListApi([])
            worker = mock.Mock(return_value=3)
            result = self.dispatch(api, worker=worker)
            self.assertEqual(result, 3)
            worker.assert_called_once_with(self.root, path, 7, reconcile=True)
            self.assertEqual(api.calls, [])
        with self.subTest(phase='uncertain journal stops without reconciliation or scan'):
            self.doCleanups()
            self.setUp()
            self.write_journal("merge-7.json", "observing")
            api = ListApi([])
            worker = mock.Mock()
            self.assertEqual(
                self.dispatch(api, worker=worker), 3
            )
            worker.assert_not_called()
            self.assertEqual(api.calls, [])
        with self.subTest(phase='recorded head is never executed again'):
            self.doCleanups()
            self.setUp()
            self.write_journal("merge-7-old.json", "rejected")
            api = ListApi([pr(7, HEAD_7)])
            worker = mock.Mock()
            observe = mock.Mock()
            with mock.patch.object(merge_pr, "observe", observe):
                result = self.dispatch(api, worker=worker, fetch=lambda *_: True)
            self.assertEqual(result, 0)
            worker.assert_not_called()
            observe.assert_not_called()

    def test_scan_skips_or_stops_as_uncertain_before_invoking_a_worker(self):
        with self.subTest(phase='no ready pr does not invoke worker'):
            self.doCleanups()
            self.setUp()
            api = ListApi([pr(7, HEAD_7)])
            worker = mock.Mock()
            with mock.patch.object(merge_pr, "observe", return_value={
                "pr": 7, "head": HEAD_7, "base": "b" * 40, "errors": ["not ready"]
            }):
                result = self.dispatch(api, worker=worker, fetch=lambda *_: True)
            self.assertEqual(result, 0)
            worker.assert_not_called()
        with self.subTest(phase='non routine pr skips expensive remote admission'):
            self.doCleanups()
            self.setUp()
            api = ListApi([pr(7, HEAD_7)])
            worker = mock.Mock()
            observe = mock.Mock()
            with mock.patch.object(merge_pr, "observe", observe):
                result = self.dispatch(
                    api,
                    worker=worker,
                    fetch=lambda *_: True,
                    classify=lambda *_: ["non-routine path"],
                )
            self.assertEqual(result, 0)
            worker.assert_not_called()
            observe.assert_not_called()
        with self.subTest(phase='controller state fault during prefilter is uncertain'):
            self.doCleanups()
            self.setUp()
            api = ListApi([pr(7, HEAD_7)])
            stderr = io.StringIO()
            with mock.patch("sys.stderr", new=stderr):
                result = self.dispatch(
                    api,
                    fetch=lambda *_: True,
                    classify=mock.Mock(side_effect=ValueError("checkout moved")),
                )
            self.assertEqual(result, 3)
            self.assertEqual(json.loads(stderr.getvalue()), {
                "event": "uncertain", "stage": "classify-candidate", "pr": 7,
            })
        with self.subTest(phase='main advance during non routine scan is uncertain'):
            self.doCleanups()
            self.setUp()
            api = ListApi([pr(7, HEAD_7)], base="a" * 40)
            stderr = io.StringIO()
            with mock.patch("sys.stderr", new=stderr):
                result = self.dispatch(
                    api,
                    fetch=lambda *_: True,
                    classify=lambda *_: ["non-routine path"],
                )
            self.assertEqual(result, 3)
            self.assertEqual(json.loads(stderr.getvalue()), {
                "event": "uncertain", "stage": "main-changed",
            })
        with self.subTest(phase='fetch failure names the stage and pr'):
            self.doCleanups()
            self.setUp()
            api = ListApi([pr(7, HEAD_7)])
            stderr = io.StringIO()
            with mock.patch("sys.stderr", new=stderr):
                result = self.dispatch(api, fetch=lambda *_: False)
            self.assertEqual(result, 3)
            self.assertEqual(json.loads(stderr.getvalue()), {
                "event": "uncertain", "stage": "fetch-candidate", "pr": 7,
            })
        with self.subTest(phase='invalid later list record fails before any candidate is observed'):
            self.doCleanups()
            self.setUp()
            api = ListApi([pr(7, HEAD_7), {"id": 200, "number": "bad", "head": {"sha": HEAD_9}}])
            worker = mock.Mock()
            observe = mock.Mock()
            with mock.patch.object(merge_pr, "observe", observe):
                result = self.dispatch(api, worker=worker, fetch=lambda *_: True)
            self.assertEqual(result, 3)
            worker.assert_not_called()
            observe.assert_not_called()

    def test_one_ready_pr_runs_only_the_trusted_worker_on_its_exact_candidate(self):
        with self.subTest(phase='reuses an exact fetched candidate'):
            self.doCleanups()
            self.setUp()
            with mock.patch.object(merge_pr, "git", side_effect=[
                    (HEAD_7 + "\n").encode(), b"",
            ]) as git:
                self.assertTrue(dispatch_merge.fetch_candidate(self.root, 7, HEAD_7))
            self.assertEqual(git.call_args_list, [
                mock.call(self.root, "rev-parse", "--verify", "refs/tilo-integration/candidates/7"),
                mock.call(self.root, "cat-file", "-e", HEAD_7 + "^{commit}"),
            ])
        with self.subTest(phase='selects one ready pr by number and invokes exact worker'):
            self.doCleanups()
            self.setUp()
            api = ListApi([pr(9, HEAD_9), pr(7, HEAD_7)])
            observations = {
                7: {"pr": 7, "head": HEAD_7, "base": "b" * 40, "errors": []},
                9: {"pr": 9, "head": HEAD_9, "base": "b" * 40, "errors": []},
            }
            worker = mock.Mock(return_value=0)
            with mock.patch.object(merge_pr, "observe",
                                   side_effect=lambda _api, number, _root: observations[number]):
                result = self.dispatch(api, worker=worker, fetch=lambda *_: True)
            self.assertEqual(result, 0)
            worker.assert_called_once()
            args = worker.call_args.args
            self.assertEqual((args[0], args[2]), (self.root, 7))
            self.assertRegex(args[1].name, r"^merge-7-777777777777-.*-[0-9a-f]{16}\.json$")
            self.assertEqual(worker.call_args.kwargs, {})
        with self.subTest(phase='worker command executes only the trusted worker'):
            self.doCleanups()
            self.setUp()
            journal = self.state / "merge-7.json"
            trusted_worker = self.state / "immutable-controller" / "merge_pr.py"
            with mock.patch.object(dispatch_merge, "WORKER", trusted_worker), \
                    mock.patch.object(dispatch_merge.subprocess, "run") as run:
                run.return_value.returncode = 2
                result = dispatch_merge.run_worker(self.root, journal, 7)
            self.assertEqual(result, 2)
            run.assert_called_once_with([
                dispatch_merge.sys.executable,
                "-I",
                str(trusted_worker),
                "--pr", "7",
                "--git-root", str(self.root),
                "--journal", str(journal),
                "--execute",
            ], check=False, timeout=180)


class MainRefreshTests(unittest.TestCase):
    def setUp(self):
        # Merged tests reset this per phase with doCleanups() + setUp().
        self.repo = MainRepo()
        self.addCleanup(self.repo.close)

    def api(self, head):
        return lambda path: (200, {"commit": {"id": head}})

    def test_checkout_is_reset_only_to_exact_observed_main_when_clean(self):
        with self.subTest(phase='advances the checkout to exact observed main'):
            self.doCleanups()
            self.setUp()
            previous = command(self.repo.root, "rev-parse", "HEAD")
            current = self.repo.advance()
            self.assertNotEqual(previous, current)
            self.assertTrue(dispatch_merge.refresh_main(self.api(current), self.repo.root))
            self.assertEqual(command(self.repo.root, "rev-parse", "HEAD"), current)
            self.assertEqual(command(self.repo.root, "status", "--porcelain"), "")
        with self.subTest(phase='current exact main needs no network fetch'):
            self.doCleanups()
            self.setUp()
            current = command(self.repo.root, "rev-parse", "HEAD")
            command(self.repo.root, "remote", "set-url", "origin", "invalid://must-not-fetch")
            self.assertTrue(dispatch_merge.refresh_main(self.api(current), self.repo.root))
        with self.subTest(phase='dirty or untracked checkout is never reset'):
            self.doCleanups()
            self.setUp()
            original = command(self.repo.root, "rev-parse", "HEAD")
            current = self.repo.advance()
            (self.repo.root / "untracked.txt").write_text("keep\n")
            self.assertFalse(dispatch_merge.refresh_main(self.api(current), self.repo.root))
            self.assertEqual(command(self.repo.root, "rev-parse", "HEAD"), original)
            self.assertTrue((self.repo.root / "untracked.txt").exists())
        with self.subTest(phase='api and fetched main mismatch does not reset'):
            self.doCleanups()
            self.setUp()
            original = command(self.repo.root, "rev-parse", "HEAD")
            self.repo.advance()
            self.assertFalse(dispatch_merge.refresh_main(self.api("f" * 40), self.repo.root))
            self.assertEqual(command(self.repo.root, "rev-parse", "HEAD"), original)


class CredentialBindingTests(unittest.TestCase):
    def test_dispatcher_rejects_missing_or_wrong_fingerprint_before_api_construction(self):
        token = "repository-scoped-test-token"
        good = hashlib.sha256(token.encode()).hexdigest()
        for fingerprint in (None, "0" * 64):
            with self.subTest(fingerprint=fingerprint), tempfile.TemporaryDirectory() as state, \
                    mock.patch.object(merge_pr, "Api", side_effect=AssertionError("API called")), \
                    mock.patch("sys.stderr", new=io.StringIO()):
                environ = {"FORGEJO_TOKEN": token}
                if fingerprint is not None:
                    environ["FORGEJO_TOKEN_SHA256"] = fingerprint
                with self.assertRaises(SystemExit) as raised:
                    dispatch_merge.main([
                        "--git-root", str(ROOT), "--state-dir", state, "--execute",
                    ], environ)
                self.assertEqual(raised.exception.code, 2)
        self.assertEqual(merge_pr.bound_token({
            "FORGEJO_TOKEN": token, "FORGEJO_TOKEN_SHA256": good,
        }), token)


if __name__ == "__main__":
    unittest.main()
