"""Exercise the changed-file rustfmt gate against real divergent/shallow Git history."""
from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

from git_fixture import isolated_git_environment

CI = Path(__file__).resolve().parents[1] / "ci"


class ChangedRustComparisonTests(unittest.TestCase):
    def setUp(self):
        environment = mock.patch.dict(os.environ, isolated_git_environment(), clear=True)
        environment.start()
        self.addCleanup(environment.stop)
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.origin = self.directory / "origin"
        self.origin.mkdir()
        self.git(self.origin, "init", "-q", "-b", "main")
        self.git(self.origin, "config", "user.email", "test@example.invalid")
        self.git(self.origin, "config", "user.name", "Test")
        (self.origin / "legacy.rs").write_text("#[allow(dead_code)]\nfn legacy() {}\n")
        self.commit("shared ancestor")
        self.ancestor = self.git(self.origin, "rev-parse", "HEAD")
        (self.origin / "legacy.rs").write_text(
            '#[allow(dead_code, reason = "callback")]\nfn legacy() {}\n')
        self.commit("main fixes an existing allow")
        self.base = self.git(self.origin, "rev-parse", "HEAD")
        self.git(self.origin, "checkout", "-qb", "pr", self.ancestor)
        (self.origin / "notes.txt").write_text("PR only changes docs\n")
        self.commit("PR docs")
        self.bin = self.directory / "bin"
        self.bin.mkdir()
        self.arguments = self.directory / "rustfmt-arguments"
        stub = self.bin / "rustfmt"
        stub.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$RUSTFMT_ARGUMENTS"\n')
        stub.chmod(0o755)

    def git(self, root, *args):
        return subprocess.check_output(["git", *args], cwd=root, text=True,
                                       stderr=subprocess.PIPE).strip()

    def commit(self, message):
        self.git(self.origin, "add", ".")
        self.git(self.origin, "commit", "-qm", message)

    def shallow_clone(self):
        checkout = self.directory / "shallow"
        self.git(self.directory, "clone", "-q", "--depth=1", "--branch=pr",
                 self.origin.as_uri(), str(checkout))
        self.assertEqual(self.git(checkout, "rev-parse", "--is-shallow-repository"), "true")
        return checkout

    def check(self, root, base="", before=""):
        self.arguments.unlink(missing_ok=True)
        env = dict(os.environ, TILO_REPO_ROOT=str(root), RUSTFMT_ARGUMENTS=str(self.arguments),
                   PATH=str(self.bin) + os.pathsep + os.environ["PATH"])
        command = ["bash", str(CI / "check-changed-rustfmt.sh"), base, before]
        return subprocess.run(command, env=env, text=True, capture_output=True)

    def test_changed_rust_files_come_from_the_exact_observed_comparison_trees(self):
        with self.subTest(phase='advanced main does not attribute its changes to docs pr'):
            self.doCleanups()
            self.setUp()
            result = self.check(self.origin, self.base)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(self.ancestor + "..HEAD", result.stdout)
            self.assertFalse(self.arguments.exists())
        with self.subTest(phase='shallow pr fetches both endpoints until merge base is observable'):
            self.doCleanups()
            self.setUp()
            checkout = self.shallow_clone()
            result = self.check(checkout, self.base)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(self.ancestor + "..HEAD", result.stdout)
            self.assertFalse(self.arguments.exists())
            self.assertEqual(self.git(checkout, "merge-base", self.base, "HEAD"), self.ancestor)
        with self.subTest(phase='actual pr rust changes still get checked'):
            self.doCleanups()
            self.setUp()
            (self.origin / "new.rs").write_text("fn new() {}\n")
            self.commit("PR adds a Rust file")
            result = self.check(self.origin, self.base)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(self.arguments.read_text().splitlines(),
                             ["--edition", "2024", "--check", "--", "new.rs"])
        with self.subTest(phase='force push preserves exact before tree instead of merge base'):
            self.doCleanups()
            self.setUp()
            result = self.check(self.origin, "null", self.base)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("legacy.rs", self.arguments.read_text().splitlines())
        with self.subTest(phase='missing shallow ancestry fails without checking wrong tree'):
            self.doCleanups()
            self.setUp()
            checkout = self.shallow_clone()
            self.git(checkout, "fetch", "--depth=1", "origin", self.base)
            self.git(checkout, "remote", "set-url", "origin", str(self.directory / "unavailable"))
            result = self.check(checkout, self.base)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("Rust formatting OK", result.stdout)
            self.assertFalse(self.arguments.exists())
        with self.subTest(phase='diff failure cannot be mistaken for no rust changes'):
            self.doCleanups()
            self.setUp()
            real_git = shutil.which("git")
            stub = self.bin / "git"
            stub.write_text('#!/bin/sh\nif [ "$1" = diff ]; then exit 23; fi\n'
                            + 'exec "' + real_git + '" "$@"\n')
            stub.chmod(0o755)
            result = self.check(self.origin, self.base)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("no Rust files", result.stdout)
            self.assertFalse(self.arguments.exists())


if __name__ == "__main__":
    unittest.main()
