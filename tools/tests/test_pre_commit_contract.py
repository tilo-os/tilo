import json
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
HOOK = ROOT / ".githooks" / "pre-commit"
CHECK_WORKFLOW = ROOT / ".forgejo" / "workflows" / "check.yml"
CLIPPY_CHECK = ROOT / "tools/ci/clippy-check.sh"


class PreCommitContractTests(unittest.TestCase):
    def test_local_hook_stays_non_compiling_while_ci_owns_absolute_clippy(self) -> None:
        with self.subTest(phase='local hook remains non compiling'):
            hook = HOOK.read_text(encoding="utf-8")

            for command in ("cargo check", "cargo clippy", "cargo test", "cargo nextest"):
                self.assertNotIn(command, hook)
        with self.subTest(phase='local hook checks staged allow reasons'):
            hook = HOOK.read_text(encoding="utf-8")

            self.assertIn("check_changed_rust_allows.py", hook)
            self.assertIn("--staged", hook)
        with self.subTest(phase='local hook admits only declared lock updates'):
            hook = HOOK.read_text(encoding="utf-8")

            self.assertIn('python3 "$root/tools/ci/check_lock_update.py" --root "$root" || fail=1', hook)
            self.assertIn("TILO_CARGO_UPDATE=", hook)
        with self.subTest(phase='ci owns the absolute clippy gate'):
            workflow = CHECK_WORKFLOW.read_text(encoding="utf-8")

            self.assertIn("tools/ci/clippy-check.sh ${{ matrix.packages }}", workflow)
            self.assertNotIn("clippy-diff", workflow)
            self.assertNotIn("clippy-base", workflow)
        with self.subTest(phase='clippy denies every workspace warning'):
            script = CLIPPY_CHECK.read_text(encoding="utf-8")

            self.assertIn("cargo clippy --locked --all-targets", script)
            self.assertIn("-- -D warnings", script)
            self.assertNotIn("--cap-lints", script)
        with self.subTest(phase='clippy does not require os only native sdks'):
            workflow = CHECK_WORKFLOW.read_text(encoding="utf-8")

            invocation = next(
                line
                for line in workflow.splitlines()
                if "tools/ci/clippy-check.sh" in line
            )
            self.assertNotIn("--all-features", invocation)
            self.assertIn("native SDKs", workflow)
            self.assertIn("release-app workflow", workflow)
        with self.subTest(phase='clippy runs once per selected package set'):
            workflow = CHECK_WORKFLOW.read_text(encoding="utf-8")
            step = workflow.split("- name: Clippy shard", 1)[1].split("- name:", 1)[0]

            self.assertIn("if: matrix.arch == 'x86_64'", step)
        with self.subTest(phase='non diagnostic failure retries without cache hits'):
            script = CLIPPY_CHECK.read_text(encoding="utf-8")

            self.assertIn("if has_compiler_diagnostics; then", script)
            self.assertIn('SCCACHE_RECACHE=1 run_clippy "$@"', script)
            self.assertIn('tail -160 "$stderr_log"', script)


class PreCommitMergeContentTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.git("init", "-q", "-b", "main")
        # Run the real hook explicitly below, not while constructing history.
        hooks = self.root / "empty-hooks"
        hooks.mkdir()
        self.git("config", "core.hooksPath", str(hooks))
        self.git("config", "commit.gpgsign", "false")
        hook = HOOK.read_text(encoding="utf-8")
        real_checks = {
            "tools/policy/check_source_content.py",
            "tools/policy/check_public_references.py",
            "tools/policy/added_line_policy.py",
            "tools/ci/check_changed_rust_allows.py",
        }
        # Keep the production hook and content/changed-line security checks.
        # Workspace/UI/interpreter inventories are unrelated to this tiny repo.
        for name in re.findall(r'python3 "\$root/([^"]+)"', hook):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            if name in real_checks:
                shutil.copy2(ROOT / name, path)
            else:
                path.write_text("# Unrelated domain policy fixture.\n", encoding="utf-8")
        path = self.root / "scripts/check-workspace-complete.sh"
        path.parent.mkdir(parents=True)
        path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        path.chmod(0o755)
        path = self.root / ".githooks/pre-commit"
        path.parent.mkdir()
        shutil.copy2(HOOK, path)

        # The old branch predates the exceptions policy and still has an asset
        # that main removed. Filtering inherited changes resurrects that asset
        # and removes the manifest, producing a tree no one intends to commit.
        (self.root / "obsolete.bin").write_bytes(b"\0obsolete")
        self.commit("test: establish old source tree")
        base = self.git("rev-parse", "HEAD").stdout.strip()
        self.git("rm", "obsolete.bin")
        (self.root / "inherited.bin").write_bytes(b"\0pinned asset")
        self.git("add", "inherited.bin")
        oid = self.git("rev-parse", ":inherited.bin").stdout.strip()
        self.manifest([{"path": "inherited.bin", "oid": oid,
                        "reason": "Required upstream fixture"}])
        # An inherited changed-line violation must remain outside the task's
        # filtered diff, while the whole-tree content audit still includes it.
        (self.root / "inherited.rs").write_text('fn main() { dbg!(1); }\n')
        self.commit("test: introduce pinned asset policy")
        self.git("checkout", "-qb", "task", base)
        (self.root / "task.txt").write_text("task source\n")
        self.commit("test: change task source")
        self.git("-c", "user.name=Test", "-c", "user.email=test@example.org",
                 "merge", "--no-commit", "main")

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.root), *args],
                              capture_output=True, text=True, check=True)

    def commit(self, message):
        self.git("add", "--", ".")
        self.git("-c", "user.name=Test", "-c", "user.email=test@example.org",
                 "commit", "-qm", message)

    def manifest(self, entries):
        path = self.root / "tools/policy/source_content_exceptions.json"
        path.write_text(json.dumps(entries) + "\n", encoding="utf-8")
        self.git("add", "--", str(path.relative_to(self.root)))

    def run_hook(self):
        before = self.git("write-tree").stdout
        result = subprocess.run(["bash", str(self.root / ".githooks/pre-commit")],
                                cwd=self.root, capture_output=True, text=True)
        self.assertEqual(self.git("write-tree").stdout, before,
                         "the real candidate index must not be modified")
        return result

    def test_merge_audits_real_tree_before_filtering_inherited_manifest(self):
        self.assertNotIn("tools/policy/source_content_exceptions.json",
                         self.git("ls-tree", "-r", "--name-only", "HEAD").stdout)
        self.assertIn("tools/policy/source_content_exceptions.json",
                      self.git("ls-files").stdout)
        result = self.run_hook()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_merge_rejects_new_staged_binary_even_with_exception(self):
        path = self.root / "new.bin"
        path.write_bytes(b"\0forbidden new asset")
        self.git("add", "new.bin")
        inherited = json.loads((self.root / "tools/policy/source_content_exceptions.json").read_text())
        oid = self.git("rev-parse", ":new.bin").stdout.strip()
        self.manifest(inherited + [{"path": "new.bin", "oid": oid,
                                    "reason": "A new pin cannot authorize new bytes"}])
        path.write_text("worktree bytes must not hide the staged binary\n")
        result = self.run_hook()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("new.bin: non-text content; new/modified non-text or non-LF blobs are forbidden",
                      result.stdout + result.stderr)

    def test_merge_still_rejects_task_changed_line_violations(self):
        path = self.root / "task.rs"
        path.write_text('fn main() { dbg!(2); }\n')
        self.git("add", "task.rs")
        result = self.run_hook()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("dbg!", result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
