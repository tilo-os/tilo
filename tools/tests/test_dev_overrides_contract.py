import tempfile
import unittest
from pathlib import Path

from tools.policy.check_dev_overrides_contract import audit


class DevOverridesContractTests(unittest.TestCase):
    def test_production_builds_are_free_of_dev_override_features(self):
        with self.subTest(phase='reports features in production builds with location and fix'):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                os_dir = root / "os"
                os_dir.mkdir()
                (os_dir / "Containerfile").write_text(
                    "FROM base\n"
                    "# dev-overrides must never be enabled here\n"
                    "RUN cargo build --release -p shell --features dev-overrides \\\n"
                    "    # --features dev-session in a continuation comment is fine\n"
                    "    && cargo build --release -p shell --features shell/dev-session\n"
                    "RUN cargo build --release --all-features\n"
                    "RUN cargo build --release -p shell --features my-dev-overrides-x\n",
                    encoding="utf-8",
                )
                (os_dir / "Containerfile.dev").write_text(
                    "RUN cargo build --release -p shell --features dev-overrides\n",
                    encoding="utf-8",
                )

                violations = audit(os_dir, root)

            self.assertEqual(
                [(v.path, v.line, v.token) for v in violations],
                [
                    ("os/Containerfile", 3, "dev-overrides"),
                    ("os/Containerfile", 5, "dev-session"),
                    ("os/Containerfile", 6, "--all-features"),
                ],
            )
            message = str(violations[0])
            self.assertIn("os/Containerfile:3:", message)
            self.assertIn("Why:", message)
            self.assertIn("Fix:", message)
        with self.subTest(phase='repository production containerfiles are clean'):
            self.assertEqual([str(v) for v in audit()], [])


if __name__ == "__main__":
    unittest.main()
