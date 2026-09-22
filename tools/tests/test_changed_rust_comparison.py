"""Exercise both lint entrypoints against real divergent/shallow Git history."""
from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

CI = Path(__file__).resolve().parents[1] / "ci"


class ChangedRustComparisonTests(unittest.TestCase):
    def setUp(self):
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

    def check(self, entrypoint, root, base="", before=""):
        self.arguments.unlink(missing_ok=True)
        env = dict(os.environ, TILO_REPO_ROOT=str(root), RUSTFMT_ARGUMENTS=str(self.arguments),
                   PATH=str(self.bin) + os.pathsep + os.environ["PATH"])
        if entrypoint == "allows":
            command = [sys.executable, str(CI / "check_changed_rust_allows.py"),
                       "--root", str(root), base, before]
        else:
            command = ["bash", str(CI / "check-changed-rustfmt.sh"), base, before]
        return subprocess.run(command, env=env, text=True, capture_output=True)

    def test_advanced_main_does_not_attribute_its_changes_to_docs_pr(self):
        for entrypoint in ("allows", "format"):
            with self.subTest(entrypoint=entrypoint):
                result = self.check(entrypoint, self.origin, self.base)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(self.ancestor + "..HEAD", result.stdout)
                self.assertFalse(self.arguments.exists())

    def test_shallow_pr_fetches_both_endpoints_until_merge_base_is_observable(self):
        checkout = self.shallow_clone()
        # Run the formatter first so its production range resolver must recover.
        for entrypoint in ("format", "allows"):
            with self.subTest(entrypoint=entrypoint):
                result = self.check(entrypoint, checkout, self.base)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(self.ancestor + "..HEAD", result.stdout)
                self.assertFalse(self.arguments.exists())
        self.assertEqual(self.git(checkout, "merge-base", self.base, "HEAD"), self.ancestor)

    def test_actual_pr_rust_changes_still_get_checked(self):
        (self.origin / "new.rs").write_text("#[allow(dead_code)]\nfn new() {}\n")
        self.commit("PR adds an unexplained allow")
        result = self.check("allows", self.origin, self.base)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("new.rs:1:", result.stderr)
        self.assertNotIn("legacy.rs:", result.stderr)
        result = self.check("format", self.origin, self.base)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.arguments.read_text().splitlines(),
                         ["--edition", "2024", "--check", "--", "new.rs"])

    def test_staged_mode_reads_the_index_instead_of_the_worktree(self):
        staged = self.origin / "staged.rs"
        staged.write_text("#[allow(dead_code)]\nfn staged() {}\n")
        self.git(self.origin, "add", "staged.rs")
        staged.write_text(
            '#[allow(dead_code, reason = "working tree is not committed")]\n'
            "fn staged() {}\n"
        )
        result = subprocess.run(
            [
                sys.executable,
                str(CI / "check_changed_rust_allows.py"),
                "--root",
                str(self.origin),
                "--staged",
            ],
            text=True,
            capture_output=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("staged.rs:1:", result.stderr)

    def test_force_push_preserves_exact_before_tree_instead_of_merge_base(self):
        result = self.check("allows", self.origin, "", self.base)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("legacy.rs:1:", result.stderr)
        result = self.check("format", self.origin, "null", self.base)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("legacy.rs", self.arguments.read_text().splitlines())

    def test_missing_shallow_ancestry_fails_both_gates_without_checking_wrong_tree(self):
        checkout = self.shallow_clone()
        self.git(checkout, "fetch", "--depth=1", "origin", self.base)
        self.git(checkout, "remote", "set-url", "origin", str(self.directory / "unavailable"))
        for entrypoint in ("allows", "format"):
            with self.subTest(entrypoint=entrypoint):
                result = self.check(entrypoint, checkout, self.base)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("Rust formatting OK", result.stdout)
                self.assertNotIn("allow reasons OK", result.stdout)
                self.assertFalse(self.arguments.exists())

    def test_diff_failure_cannot_be_mistaken_for_no_rust_changes(self):
        real_git = shutil.which("git")
        stub = self.bin / "git"
        stub.write_text('#!/bin/sh\nif [ "$1" = diff ]; then exit 23; fi\n'
                        + 'exec "' + real_git + '" "$@"\n')
        stub.chmod(0o755)
        for entrypoint in ("allows", "format"):
            with self.subTest(entrypoint=entrypoint):
                result = self.check(entrypoint, self.origin, self.base)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("no Rust files", result.stdout)
                self.assertFalse(self.arguments.exists())


if __name__ == "__main__":
    unittest.main()
