import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class CommitHookTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name) / "repo"
        self.root.mkdir()
        self.git("init", "-q", "-b", "main")
        for key, value in (("user.name", "Test"), ("user.email", "test@example.org"), ("commit.gpgsign", "false")):
            self.git("config", key, value)
        paths = ["tools/policy/check_source_content.py", "tools/policy/check_commit_message.py", "tools/policy/check_self_contained.py", "tools/policy/check_commit_policy.py", "tools/ci/check_changed_rust_allows.py", "tools/dev/install-git-hooks.py", ".githooks/commit-msg"]
        for name in paths:
            dest = self.root / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / name, dest)
        # Exercise the production content checker through Git; the unrelated
        # Rust/workspace checks in the full hook have their own contract tests.
        pre = self.root / ".githooks/pre-commit"
        pre.write_text('#!/bin/sh\nexec python3 "$(git rev-parse --show-toplevel)/tools/policy/check_source_content.py" --staged\n')
        pre.chmod(0o755)
        self.manifest([])
        self.git("add", "--", ".")
        self.git("commit", "--no-verify", "-qm", "test: establish text fixture")
        self.base = self.git("rev-parse", "HEAD").stdout.strip()
        self.run_installer()

    def git(self, *args, check=True):
        return subprocess.run(["git", "-C", str(self.root), *args], capture_output=True, text=True, check=check)

    def run_installer(self, check=True, root=None):
        root = root or self.root
        return subprocess.run(["python3", str(root / "tools/dev/install-git-hooks.py")], cwd=root, capture_output=True, text=True, check=check)

    def manifest(self, entries):
        path = self.root / "tools/policy/source_content_exceptions.json"
        path.write_text(json.dumps(entries) + "\n")
        self.git("add", "--", str(path.relative_to(self.root)))

    def stage(self, name, data):
        (self.root / name).write_bytes(data)
        self.git("add", "--", name)

    def ci(self, head_ref=None):
        env = dict(os.environ)
        if head_ref is not None:
            env["PR_HEAD_REF"] = head_ref
        return subprocess.run(["python3", str(self.root / "tools/policy/check_commit_policy.py"), "", self.base], cwd=self.root, capture_output=True, text=True, env=env)

    def test_subject_scope_breaking_marker_and_multiline_body(self):
        self.stage("source", b"text\n")
        self.git("commit", "-qm", "feat(shell)!: expose actions\n\nSeveral paragraphs are valid.\n\nBREAKING CHANGE: callers now use resources.")
        self.assertEqual(self.ci().returncode, 0)

    def test_malformed_message_is_rejected_with_actionable_examples(self):
        self.stage("source", b"text\n")
        before = self.git("rev-parse", "HEAD").stdout
        result = self.git("commit", "-qm", "update stuff", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("fix(shell): preserve window identity", result.stderr)
        self.assertEqual(self.git("rev-parse", "HEAD").stdout, before)

    def test_pull_request_head_branch_with_a_tracker_id_is_rejected(self):
        self.stage("source", b"text\n")
        self.git("commit", "-qm", "fix(ui): handle focus")
        # Spelled in pieces so this file does not trip the check it tests.
        bad = "bob/ti" + "l-" + "42-focus"
        result = self.ci(head_ref=bad)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("branch name", result.stderr)
        self.assertEqual(self.ci(head_ref="fix/focus-handling").returncode, 0)

    def test_internal_references_in_subject_or_body_are_rejected(self):
        self.stage("source", b"text\n")
        # Spelled in pieces so this file does not trip the check it tests.
        ident = "TI" + "L-" + "42"
        for message in ("fix(ui): handle focus (" + ident + ")", "fix(ui): handle focus\n\nSee https://linear" + ".app/x/issue/" + ident):
            with self.subTest(message=message):
                result = self.git("commit", "-qm", message, check=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("internal reference", result.stderr)

    def test_unknown_type_empty_scope_and_missing_body_separator_fail(self):
        self.stage("source", b"text\n")
        for message in ("unknown(ui): change", "fix(): change", "fix( ): change", "fix(ui): change\nBody without a separator", "fix(ui):"):
            with self.subTest(message=message):
                self.assertNotEqual(self.git("commit", "-qm", message, check=False).returncode, 0)

    def test_staged_content_wins_over_worktree_and_handles_spaces(self):
        self.stage("name with spaces.txt", b"bad\r\n")
        (self.root / "name with spaces.txt").write_bytes(b"fixed\n")
        result = self.git("commit", "-qm", "fix: correct line endings", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("name with spaces.txt: non-LF", result.stderr)
        self.stage("name with spaces.txt", b"fixed\n")
        (self.root / "name with spaces.txt").write_bytes(b"\0dirty worktree")
        self.git("commit", "-qm", "fix: stage correct bytes")
        self.assertEqual(self.git("show", "HEAD:name with spaces.txt").stdout, "fixed\n")

    def test_nul_invalid_utf8_and_bare_cr_do_not_depend_on_extension(self):
        for data in (b"\0binary", b"\xff", b"line\rend"):
            with self.subTest(data=data):
                self.stage("source.rs", data)
                result = self.git("commit", "-qm", "test: exercise content", check=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("new/modified non-text or non-LF blobs are forbidden", result.stderr)

    def test_added_exception_cannot_admit_new_binary(self):
        self.stage("new.bin", b"\0binary")
        oid = self.git("rev-parse", ":new.bin").stdout.strip()
        self.manifest([{"path": "new.bin", "oid": oid, "reason": "Not a permission to add binary"}])
        self.assertNotEqual(self.git("commit", "-qm", "test: add exception", check=False).returncode, 0)
        self.git("commit", "--no-verify", "-qm", "test: bypass hook")
        result = self.ci()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("new.bin: non-text", result.stderr)

    def test_unchanged_legacy_binary_allowed_modified_binary_rejected_and_deletion_allowed(self):
        self.stage("legacy.bin", b"\0original")
        oid = self.git("rev-parse", ":legacy.bin").stdout.strip()
        self.manifest([{"path": "legacy.bin", "oid": oid, "reason": "Established legacy fixture"}])
        self.git("commit", "--no-verify", "-qm", "test: establish legacy baseline")
        self.base = self.git("rev-parse", "HEAD").stdout.strip()
        self.stage("source", b"text\n")
        self.git("commit", "-qm", "docs: retain unchanged legacy asset")
        self.stage("legacy.bin", b"\0replacement")
        oid = self.git("rev-parse", ":legacy.bin").stdout.strip()
        self.manifest([{"path": "legacy.bin", "oid": oid, "reason": "Changed pin does not authorize bytes"}])
        self.assertNotEqual(self.git("commit", "-qm", "test: replace asset", check=False).returncode, 0)
        self.git("rm", "-f", "--", "legacy.bin")
        self.git("commit", "-qm", "chore: remove legacy asset")
        self.assertEqual(self.ci().returncode, 0)

    def test_ci_catches_bypassed_message_and_intermediate_binary_additions(self):
        self.stage("source", b"text\n")
        self.git("commit", "--no-verify", "-qm", "not conventional")
        self.stage("added.bin", b"\0payload")
        self.git("commit", "--no-verify", "-qm", "test: add payload")
        self.git("rm", "--", "added.bin")
        self.git("commit", "--no-verify", "-qm", "test: remove payload")
        result = self.ci()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Use type(optional-scope)", result.stderr)
        self.assertIn("added.bin: non-text", result.stderr)

    def test_ci_batch_reader_checks_large_and_intermediate_blobs(self):
        # Exceed pipe capacity, mix empty and repeated blobs, and restore/remove
        # forbidden bytes later. Checking only the final tree must still fail.
        for index in range(64):
            self.stage(f"text {index}.txt", ("UTF-8 text λ\n" * 8192).encode())
        self.stage("empty.txt", b"")
        self.stage("intermediate.bin", b"forbidden\0bytes\n")
        self.stage("restored.txt", b"forbidden\r\n")
        self.git("commit", "--no-verify", "-qm", "test: establish large intermediate changes")
        self.git("rm", "--", "intermediate.bin")
        self.stage("restored.txt", b"valid final text\n")
        self.git("commit", "-qm", "fix: restore final text")
        result = self.ci()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("intermediate.bin: non-text", result.stderr)
        self.assertIn("restored.txt: non-LF", result.stderr)

    def test_ci_batch_reader_rejects_missing_and_non_blob_objects(self):
        for oid in ("a" * 40, self.base):
            with self.subTest(oid=oid):
                result = subprocess.run(
                    ["python3", "-c",
                     "from pathlib import Path; import sys; from check_commit_policy import blob_reader\n"
                     "with blob_reader(Path(sys.argv[1])) as read: read(sys.argv[2].encode())",
                     str(self.root), oid],
                    cwd=ROOT / "tools/policy", capture_output=True, text=True, timeout=10)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Cannot inspect exact changed blob", result.stderr)

    def test_genuine_generated_merge_revert_fixup_and_codegen_messages(self):
        self.stage("source", b"first\n")
        self.git("commit", "-qm", "fix(shell): correct source")
        target = self.git("rev-parse", "HEAD").stdout.strip()
        self.git("revert", "--no-edit", target)
        self.git("switch", "-qc", "feature")
        self.stage("feature", b"feature\n")
        self.git("commit", "-qm", "feat(ui): add feature")
        self.git("switch", "-q", "main")
        self.stage("main", b"main\n")
        self.git("commit", "-qm", "chore(codegen): refresh bindings")
        self.git("merge", "--no-ff", "feature", "-m", "Merge branch 'feature'")
        self.stage("main", b"changed\n")
        self.git("commit", "-qm", "fix(ui): correct bindings")
        self.stage("main", b"again\n")
        self.git("commit", "--fixup=HEAD", "-q")
        self.assertEqual(self.ci().returncode, 0)

    def test_unobservable_new_history_fails_instead_of_passing(self):
        self.stage("source", b"text\n")
        self.git("commit", "-qm", "fix: create valid change")
        self.stage("source", b"next\n")
        self.git("commit", "-qm", "fix: create another change")
        head = self.git("rev-parse", "HEAD").stdout.strip()
        # Preserve the objects but mark the new commit shallow: a mere
        # object-existence check would otherwise incorrectly pass this case.
        (self.root / ".git/shallow").write_text(head + "\n")
        result = self.ci()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Incomplete new history", result.stderr)

    def test_fake_merge_on_ordinary_commit_fails_locally_and_in_ci(self):
        self.stage("source", b"text\n")
        self.assertNotEqual(self.git("commit", "-qm", "Merge branch 'fake'", check=False).returncode, 0)
        self.git("commit", "--no-verify", "-qm", "Merge branch 'fake'")
        self.assertNotEqual(self.ci().returncode, 0)

    def test_installer_preserves_unrelated_hooks_and_is_idempotent(self):
        self.assertIn("verified", self.run_installer().stdout)
        self.git("config", "--local", "core.hooksPath", "custom-hooks")
        custom = self.root / "custom-hooks"
        custom.mkdir()
        hook = custom / "commit-msg"
        hook.write_text("user hook\n")
        result = self.run_installer(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.git("config", "--get", "core.hooksPath").stdout.strip(), "custom-hooks")
        self.assertEqual(hook.read_text(), "user hook\n")

    def test_installer_preserves_custom_default_hook(self):
        self.git("config", "--unset", "core.hooksPath")
        hook = self.root / ".git/hooks/commit-msg"
        hook.write_text("user hook\n")
        self.assertNotEqual(self.run_installer(check=False).returncode, 0)
        self.assertEqual(hook.read_text(), "user hook\n")
        self.assertNotEqual(self.git("config", "--get", "core.hooksPath", check=False).returncode, 0)

    def test_worktree_install_does_not_change_common_configuration(self):
        self.git("config", "extensions.worktreeConfig", "true")
        self.git("config", "--local", "core.hooksPath", str(self.root / ".githooks"))
        linked = self.root.parent / "linked"
        self.git("worktree", "add", "-qb", "linked", str(linked))
        self.assertIn("--worktree", self.run_installer(root=linked).stdout)
        self.assertEqual(self.git("config", "--local", "--get", "core.hooksPath").stdout.strip(), str(self.root / ".githooks"))
        configured = subprocess.check_output(["git", "-C", str(linked), "config", "--get", "core.hooksPath"], text=True).strip()
        self.assertEqual(configured, ".githooks")


if __name__ == "__main__":
    unittest.main()
