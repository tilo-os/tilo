import os
import pathlib
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "tools/security-boundary-invariants.txt"
RUNNER = ROOT / "tools/run-security-boundary-invariants.sh"


def manifest_entries() -> list[str]:
    return [
        line.strip()
        for line in MANIFEST.read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]


class SecurityBoundaryInvariantSuiteTests(unittest.TestCase):
    def test_every_invariant_runs_and_the_failed_ones_are_reported_as_tests(self):
        with tempfile.TemporaryDirectory() as tree:
            root = pathlib.Path(tree)
            (root / "first.sh").write_text("echo first breaks\nexit 3\n", encoding="utf-8")
            (root / "second.py").write_text("print('second holds')\n", encoding="utf-8")
            (root / "manifest.txt").write_text("first.sh\nsecond.py\n", encoding="utf-8")
            result = subprocess.run(
                ["bash", str(RUNNER)], capture_output=True, text=True, check=False,
                env={**os.environ, "TILO_REPO_ROOT": tree, "TILO_SECURITY_INVARIANT_MANIFEST": "manifest.txt"})
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        lines = result.stdout.splitlines()
        self.assertIn("second holds", lines)
        self.assertIn("test first.sh ... FAILED", lines)
        self.assertIn("test second.py ... ok", lines)
        report = lines.index("---- first.sh stdout ----")
        self.assertEqual(lines[report + 1:report + 3], ["first breaks", "first.sh exited with 3"])
        self.assertIn("test result: FAILED. 1 passed; 1 failed", lines)


if __name__ == "__main__":
    unittest.main()
