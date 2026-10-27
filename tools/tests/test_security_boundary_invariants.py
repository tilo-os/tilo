import pathlib
import subprocess
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


if __name__ == "__main__":
    unittest.main()
