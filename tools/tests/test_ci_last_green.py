from pathlib import Path
import subprocess
import tempfile
import unittest

from tools.ci_last_green import select_base


class LastGreenTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.repo = Path(self.tmp.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Test")
        self.git("config", "user.email", "test@example.invalid")
        self.first = self.commit("first")

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.repo), *args],
                                       text=True, encoding="utf-8", stderr=subprocess.PIPE).strip()

    def commit(self, name):
        (self.repo / name).write_text(name)
        self.git("add", name)
        self.git("commit", "-qm", name)
        return self.git("rev-parse", "HEAD")

    def receipt(self, sha, value=None):
        value = value or f"select=success security=success affected=success run=42 sha={sha}"
        self.git("notes", "--ref=tilo-check-ci", "add", "-f", "-m", value, sha)

    def test_cancelled_intervening_commits_are_in_diff(self):
        self.receipt(self.first)
        self.commit("cancelled")
        self.commit("latest")
        result = select_base(self.repo)
        self.assertFalse(result["full"])
        self.assertEqual(result["base"], self.first)
        self.assertEqual(set(self.git("diff", "--name-only", result["base"], "HEAD").splitlines()),
                         {"cancelled", "latest"})

    def test_newest_successful_ancestor_wins_but_current_head_never_does(self):
        self.receipt(self.first)
        second = self.commit("second")
        self.receipt(second)
        self.receipt(self.commit("latest"))
        self.assertEqual(select_base(self.repo)["base"], second)

    def test_no_receipts_and_root_receipt_require_full(self):
        self.assertTrue(select_base(self.repo)["full"])
        self.receipt(self.first)
        self.assertTrue(select_base(self.repo)["full"])

    def test_failed_malformed_and_wrong_sha_receipts_do_not_qualify(self):
        for index, value in enumerate((f"select=success security=failure affected=success run=42 sha={self.first}",
                      f"select=success security=success affected=success run=42 sha={'0'*40}",
                      f"prefix select=success security=success affected=success run=42 sha={self.first}")):
            with self.subTest(value=value):
                self.receipt(self.first, value)
                self.assertTrue(select_base(self.repo, self.commit("next" + str(index)))["full"])

    def test_side_branch_receipt_is_not_a_main_baseline(self):
        self.git("checkout", "-qb", "feature")
        self.receipt(self.commit("feature"))
        self.git("checkout", "-q", "main")
        self.commit("mainchange")
        self.git("merge", "--no-ff", "-qm", "merge", "feature")
        self.assertTrue(select_base(self.repo)["full"])

    def test_fetch_failure_ignores_existing_local_receipt(self):
        self.receipt(self.first)
        self.commit("latest")
        self.assertTrue(select_base(self.repo, fetch="nonexistent-local-remote")["full"])

    def test_bounded_history_without_receipt_requires_full(self):
        self.receipt(self.first)
        self.commit("second")
        self.commit("latest")
        self.assertTrue(select_base(self.repo, limit=1)["full"])

    def test_fetch_from_local_remote_uses_remote_receipt_snapshot(self):
        with tempfile.TemporaryDirectory() as remote:
            subprocess.check_call(["git", "init", "--bare", "-q", remote])
            self.receipt(self.first)
            self.git("push", "-q", remote, "refs/notes/tilo-check-ci")
            self.git("notes", "--ref=tilo-check-ci", "remove", self.first)
            self.commit("latest")
            self.assertEqual(select_base(self.repo, fetch=remote)["base"], self.first)


if __name__ == "__main__":
    unittest.main()
