import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
HOOK = ROOT / ".githooks" / "pre-commit"
CHECK_WORKFLOW = ROOT / ".forgejo" / "workflows" / "check.yml"
CLIPPY_CHECK = ROOT / "tools/ci/clippy-check.sh"


class PreCommitContractTests(unittest.TestCase):
    def test_local_hook_remains_non_compiling(self) -> None:
        hook = HOOK.read_text(encoding="utf-8")

        for command in ("cargo check", "cargo clippy", "cargo test", "cargo nextest"):
            self.assertNotIn(command, hook)

    def test_local_hook_checks_staged_allow_reasons(self) -> None:
        hook = HOOK.read_text(encoding="utf-8")

        self.assertIn("check_changed_rust_allows.py", hook)
        self.assertIn("--staged", hook)

    def test_ci_owns_the_absolute_clippy_gate(self) -> None:
        workflow = CHECK_WORKFLOW.read_text(encoding="utf-8")

        self.assertIn("tools/ci/clippy-check.sh ${{ matrix.packages }}", workflow)
        self.assertNotIn("clippy-diff", workflow)
        self.assertNotIn("clippy-base", workflow)

    def test_clippy_denies_every_workspace_warning(self) -> None:
        script = CLIPPY_CHECK.read_text(encoding="utf-8")

        self.assertIn("cargo clippy --locked --all-targets", script)
        self.assertIn("-- -D warnings", script)
        self.assertNotIn("--cap-lints", script)

    def test_clippy_does_not_require_os_only_native_sdks(self) -> None:
        workflow = CHECK_WORKFLOW.read_text(encoding="utf-8")

        invocation = next(
            line
            for line in workflow.splitlines()
            if "tools/ci/clippy-check.sh" in line
        )
        self.assertNotIn("--all-features", invocation)
        self.assertIn("ui/webview-wpe", workflow)
        self.assertIn("release-app workflow", workflow)

    def test_clippy_runs_once_per_selected_package_set(self) -> None:
        workflow = CHECK_WORKFLOW.read_text(encoding="utf-8")
        step = workflow.split("- name: Clippy shard", 1)[1].split("- name:", 1)[0]

        self.assertIn("if: matrix.arch == 'x86_64'", step)

    def test_non_diagnostic_failure_retries_without_cache_hits(self) -> None:
        script = CLIPPY_CHECK.read_text(encoding="utf-8")

        self.assertIn("if has_compiler_diagnostics; then", script)
        self.assertIn('SCCACHE_RECACHE=1 run_clippy "$@"', script)
        self.assertIn('tail -160 "$stderr_log"', script)


if __name__ == "__main__":
    unittest.main()
