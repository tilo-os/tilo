import os
import pathlib
import subprocess
import tempfile
import tomllib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "tools/security-boundary-invariants.txt"
RUNNER = ROOT / "tools/run-security-boundary-invariants.sh"
T_TOML = ROOT / "t.toml"


def manifest_entries() -> list[str]:
    return [
        line.strip()
        for line in MANIFEST.read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]


class SecurityBoundaryInvariantSuiteTests(unittest.TestCase):
    def test_invariant_manifest_runner_and_check_agree(self):
        with self.subTest(phase='manifest is unique and resolves to supported files'):
            entries = manifest_entries()
            self.assertGreater(len(entries), 50)
            self.assertEqual(len(entries), len(set(entries)))
            for entry in entries:
                self.assertFalse(pathlib.PurePosixPath(entry).is_absolute())
                self.assertNotIn("..", pathlib.PurePosixPath(entry).parts)
                self.assertIn(pathlib.PurePosixPath(entry).suffix, {".py", ".sh"})
                self.assertTrue((ROOT / entry).is_file(), entry)
        with self.subTest(phase='runner lists the exact manifest order'):
            result = subprocess.run(
                ["bash", str(RUNNER), "--list"],
                cwd=ROOT,
                check=True,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.stdout.splitlines(), manifest_entries())
        with self.subTest(phase='the check delegates the suite to the runner'):
            checks = tomllib.loads(T_TOML.read_text(encoding="utf-8"))["check"]
            [security] = [check for check in checks if check["name"] == "security"]
            self.assertEqual(security["run"], "bash tools/run-security-boundary-invariants.sh")
            config = T_TOML.read_text(encoding="utf-8")
            for entry in manifest_entries():
                self.assertNotIn(f"bash {entry}", config)

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
