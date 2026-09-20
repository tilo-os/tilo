import pathlib
import subprocess
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "tools/security-boundary-invariants.txt"
RUNNER = ROOT / "tools/run-security-boundary-invariants.sh"
WORKFLOW = ROOT / ".github/workflows/check.yml"


def manifest_entries() -> list[str]:
    return [
        line.strip()
        for line in MANIFEST.read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]


class SecurityBoundaryInvariantSuiteTests(unittest.TestCase):
    def test_manifest_is_unique_and_resolves_to_supported_files(self) -> None:
        entries = manifest_entries()
        self.assertGreater(len(entries), 50)
        self.assertEqual(len(entries), len(set(entries)))
        for entry in entries:
            self.assertFalse(pathlib.PurePosixPath(entry).is_absolute())
            self.assertNotIn("..", pathlib.PurePosixPath(entry).parts)
            self.assertIn(pathlib.PurePosixPath(entry).suffix, {".py", ".sh"})
            self.assertTrue((ROOT / entry).is_file(), entry)

    def test_runner_lists_the_exact_manifest_order(self) -> None:
        result = subprocess.run(
            ["bash", str(RUNNER), "--list"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.stdout.splitlines(), manifest_entries())

    def test_workflow_delegates_the_suite_to_the_runner(self) -> None:
        workflow = WORKFLOW.read_text(encoding="utf-8")
        self.assertIn("bash tools/run-security-boundary-invariants.sh", workflow)
        for entry in manifest_entries():
            self.assertNotIn(entry, workflow)


if __name__ == "__main__":
    unittest.main()
