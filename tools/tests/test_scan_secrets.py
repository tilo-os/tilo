from pathlib import Path
import subprocess
import tempfile
import unittest


SCANNER = Path(__file__).resolve().parents[1] / "policy/scan-secrets.sh"


class SecretScannerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def scan(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(SCANNER), *arguments],
            cwd=self.root,
            check=False,
            capture_output=True,
            text=True,
        )

    def stage(self, name: str, contents: str) -> None:
        (self.root / name).write_text(contents, encoding="utf-8")
        subprocess.run(["git", "add", name], cwd=self.root, check=True)

    def test_staged_credential_shape_fails_with_its_category(self) -> None:
        access_key = "AKIA" + "ABCDEFGHIJKLMNOP"
        self.stage("config.env", f"AWS_ACCESS_KEY_ID={access_key}\n")

        result = self.scan()

        self.assertEqual(result.returncode, 1)
        self.assertIn("possible AWS access key", result.stderr)
        self.assertIn("config.env", result.stderr)

    def test_explicit_example_on_the_same_line_is_allowed(self) -> None:
        access_key = "AKIA" + "ABCDEFGHIJKLMNOP"
        self.stage("example.env", f"AWS_ACCESS_KEY_ID={access_key} # example only\n")

        result = self.scan()

        self.assertEqual(result.returncode, 0, result.stderr)

    def test_full_audit_scans_every_tracked_file(self) -> None:
        token = "ghp_" + "a" * 36
        self.stage("credentials.txt", f"token={token}\n")

        result = self.scan("--all")

        self.assertEqual(result.returncode, 1)
        self.assertIn("possible GitHub token", result.stderr)


if __name__ == "__main__":
    unittest.main()
