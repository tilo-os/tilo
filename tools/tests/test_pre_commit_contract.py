import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
HOOK = ROOT / ".githooks" / "pre-commit"
CHECK_WORKFLOW = ROOT / ".forgejo" / "workflows" / "check.yml"
CLIPPY_DIFF = ROOT / "tools" / "clippy-diff.sh"


class PreCommitContractTests(unittest.TestCase):
    def test_local_hook_remains_non_compiling(self) -> None:
        hook = HOOK.read_text(encoding="utf-8")

        for command in ("cargo check", "cargo clippy", "cargo test", "cargo nextest"):
            self.assertNotIn(command, hook)

    def test_ci_owns_the_differential_clippy_gate(self) -> None:
        workflow = CHECK_WORKFLOW.read_text(encoding="utf-8")

        self.assertIn("tools/clippy-diff.sh", workflow)
        self.assertIn("Clippy diagnostic diff", workflow)

    def test_differential_clippy_covers_opt_in_features(self) -> None:
        workflow = CHECK_WORKFLOW.read_text(encoding="utf-8")

        invocation = next(
            line
            for line in workflow.splitlines()
            if "tools/clippy-diff.sh" in line
        )
        self.assertIn("--all-features", invocation)

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
        self.assertNotIn("refusing a false baseline", script)
        self.assertNotIn("accepting the base-repair path", script)


if __name__ == "__main__":
    unittest.main()
