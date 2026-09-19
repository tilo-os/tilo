"""Targeted app-release history fetches against real shallow repositories."""

import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "app_release_history", ROOT / "tools/ci/app_release_history.py"
)
history = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(history)


class AppReleaseHistory(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "source"
        self.remote = self.root / "remote.git"
        self.command("git", "init", "-q", "-b", "main", str(self.source))
        self.git(self.source, "config", "user.name", "Fixture")
        self.git(self.source, "config", "user.email", "fixture@example.invalid")

    def command(self, *args, cwd=None):
        return subprocess.check_output(
            args, cwd=cwd, stderr=subprocess.PIPE, text=True
        ).strip()

    def git(self, repository, *args):
        return self.command("git", "-C", str(repository), *args)

    def commit(self, text):
        (self.source / "file").write_text(text, encoding="utf-8")
        self.git(self.source, "add", "file")
        self.git(self.source, "commit", "-qm", text)
        return self.git(self.source, "rev-parse", "HEAD")

    def publish(self):
        self.command("git", "clone", "-q", "--bare", str(self.source), str(self.remote))

    def shallow_clone(self, ref):
        clone = self.root / ("clone-" + ref.replace("/", "-"))
        self.command(
            "git",
            "clone",
            "-q",
            "--depth=1",
            "--branch",
            ref,
            self.remote.as_uri(),
            str(clone),
        )
        return clone

    def test_fetch_base_adds_only_the_exact_comparison_commit(self):
        base = self.commit("base")
        self.commit("head")
        self.publish()
        clone = self.shallow_clone("main")
        head = self.git(clone, "rev-parse", "HEAD")

        history.fetch_exact(clone, base, history.BASE_REF)

        self.assertEqual(self.git(clone, "rev-parse", history.BASE_REF), base)
        self.assertEqual(self.git(clone, "rev-parse", "HEAD"), head)
        refs = self.git(clone, "for-each-ref", "--format=%(refname)").splitlines()
        self.assertNotIn("refs/tags", "\n".join(refs))
        self.assertIn(history.BASE_REF, refs)

    def test_tag_ancestry_deepens_only_the_observed_main_tip(self):
        candidate = ""
        for number in range(20):
            sha = self.commit(f"main-{number}")
            if number == 4:
                candidate = sha
                self.git(self.source, "tag", "app-notes-v1", candidate)
        main = self.git(self.source, "rev-parse", "HEAD")
        self.git(self.source, "checkout", "-qb", "unrelated", candidate)
        self.commit("unrelated")
        self.git(self.source, "checkout", "-q", "main")
        self.publish()
        clone = self.shallow_clone("app-notes-v1")

        self.assertEqual(history.require_main_ancestor(clone, candidate, (4, 32)), main)
        self.assertEqual(self.git(clone, "rev-parse", history.MAIN_REF), main)
        refs = self.git(clone, "for-each-ref", "--format=%(refname)")
        self.assertNotIn("refs/remotes/origin/unrelated", refs)

    def test_diverged_tag_is_rejected_after_main_history_is_complete(self):
        ancestor = self.commit("ancestor")
        self.git(self.source, "checkout", "-qb", "release", ancestor)
        candidate = self.commit("diverged release")
        self.git(self.source, "tag", "app-notes-v2", candidate)
        self.git(self.source, "checkout", "-q", "main")
        for number in range(6):
            self.commit(f"main-{number}")
        self.publish()
        clone = self.shallow_clone("app-notes-v2")

        with self.assertRaisesRegex(RuntimeError, "is not an ancestor of main"):
            history.require_main_ancestor(clone, candidate, (2, 4, 16))

    def test_invalid_or_mismatched_release_source_fails_closed(self):
        head = self.commit("head")
        self.publish()
        clone = self.shallow_clone("main")
        with self.assertRaisesRegex(ValueError, "immutable full commit ID"):
            history.require_main_ancestor(clone, "main")
        other = "0" * len(head)
        with self.assertRaisesRegex(ValueError, "does not match release source"):
            history.require_main_ancestor(clone, other)

    def test_incomplete_main_never_turns_into_a_negative_ancestry_decision(self):
        candidate = "a" * 40
        observed_main = "b" * 40
        completed = mock.Mock(stdout=candidate + "\n", stderr="", returncode=0)
        fetches = []

        with mock.patch.object(history, "git", return_value=completed), \
                mock.patch.object(history, "remote_main", return_value=observed_main), \
                mock.patch.object(
                    history, "fetch_exact", side_effect=lambda *args: fetches.append(args)
                ), \
                mock.patch.object(history, "is_ancestor", return_value=False), \
                mock.patch.object(history, "main_has_shallow_boundary", return_value=True):
            with self.assertRaisesRegex(RuntimeError, "cannot establish complete"):
                history.require_main_ancestor(Path("."), candidate, (8,))

        self.assertEqual([call[-1] if len(call) == 4 else 1 for call in fetches],
                         [1, 8, 2_147_483_647])


if __name__ == "__main__":
    unittest.main()
