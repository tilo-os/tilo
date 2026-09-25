import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
HOOK = ROOT / ".githooks" / "pre-commit"
CHECK_WORKFLOW = ROOT / ".forgejo" / "workflows" / "check.yml"
CLIPPY_DIFF = ROOT / "tools/ci/clippy-diff.sh"
CLIPPY_DIAGNOSTICS = ROOT / "tools/ci/clippy_diagnostics.py"


class PreCommitContractTests(unittest.TestCase):
    def test_local_hook_remains_non_compiling(self) -> None:
        hook = HOOK.read_text(encoding="utf-8")

        for command in ("cargo check", "cargo clippy", "cargo test", "cargo nextest"):
            self.assertNotIn(command, hook)

    def test_local_hook_checks_staged_allow_reasons(self) -> None:
        hook = HOOK.read_text(encoding="utf-8")

        self.assertIn("check_changed_rust_allows.py", hook)
        self.assertIn("--staged", hook)

    def test_ci_owns_the_differential_clippy_gate(self) -> None:
        workflow = CHECK_WORKFLOW.read_text(encoding="utf-8")

        self.assertIn("tools/ci/clippy-diff.sh", workflow)
        self.assertIn("Clippy diagnostic diff", workflow)

    def test_differential_clippy_does_not_require_os_only_native_sdks(self) -> None:
        workflow = CHECK_WORKFLOW.read_text(encoding="utf-8")

        invocation = next(
            line
            for line in workflow.splitlines()
            if "tools/ci/clippy-diff.sh" in line
        )
        self.assertNotIn("--all-features", invocation)
        self.assertIn("ui/webview-wpe", workflow)
        self.assertIn("release-app workflow", workflow)

    def test_differential_clippy_runs_once_per_selected_package_set(self) -> None:
        workflow = CHECK_WORKFLOW.read_text(encoding="utf-8")
        step = workflow.split("- name: Clippy shard", 1)[1].split("- name:", 1)[0]

        self.assertIn("if: matrix.arch == 'x86_64'", step)

    def test_manual_ci_retry_has_a_real_clippy_baseline(self) -> None:
        workflow = CHECK_WORKFLOW.read_text(encoding="utf-8")

        self.assertIn('if [ -z "$base" ] \\', workflow)
        self.assertIn("base=HEAD^", workflow)

    def test_broken_clippy_base_uses_partial_diagnostic_baseline(self) -> None:
        script = CLIPPY_DIFF.read_text(encoding="utf-8")

        head_compile = 'run_clippy "$root" "$head_json"'
        base_compile = 'run_clippy "$base_tree" "$base_json"'
        self.assertLess(script.index(head_compile), script.index(base_compile))
        self.assertIn("using its partial diagnostic baseline", script)
        self.assertIn('git -C "$root" diff --name-status -M "$base..HEAD"', script)
        self.assertIn('fingerprint[1] in changed', CLIPPY_DIAGNOSTICS.read_text(encoding="utf-8"))
        self.assertNotIn("refusing a false baseline", script)
        self.assertNotIn("accepting the base-repair path", script)

    def test_non_diagnostic_head_failure_retries_without_cache_hits(self) -> None:
        script = CLIPPY_DIFF.read_text(encoding="utf-8")

        self.assertIn('has_compiler_errors "$head_json"', script)
        self.assertIn("SCCACHE_RECACHE=1 run_clippy", script)
        self.assertIn('tail -160 "$head_stderr"', script)


if __name__ == "__main__":
    unittest.main()
