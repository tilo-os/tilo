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
            self.assertIn("ui/webview-wpe", workflow)
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


if __name__ == "__main__":
    unittest.main()
