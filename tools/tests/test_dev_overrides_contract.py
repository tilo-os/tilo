import tempfile
import unittest
from pathlib import Path

from tools.policy.check_dev_overrides_contract import audit, unread_flags


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
                    "RUN cargo build --release -p shell --features my-dev-overrides-x\n"
                    "RUN cargo build --release -p shell --features shell/hosted-test-control\n",
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
                    ("os/Containerfile", 8, "hosted-test-control"),
                ],
            )
            message = str(violations[0])
            self.assertIn("os/Containerfile:3:", message)
            self.assertIn("Why:", message)
            self.assertIn("Fix:", message)
        with self.subTest(phase='repository production containerfiles are clean'):
            self.assertEqual([str(v) for v in audit()], [])

    def test_the_repository_documents_only_controls_something_reads(self):
        self.assertEqual([str(flag) for flag in unread_flags()], [])


def documented(files: dict[str, str]) -> list[str]:
    """The flags the page in `files` documents that nothing in the tree reads."""
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        doc = root / "docs" / "performance-flags.md"
        doc.parent.mkdir(parents=True)
        doc.write_text(files.pop("docs/performance-flags.md"), encoding="utf-8")
        for name, text in files.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return [flag.flag for flag in unread_flags(root, doc)]


PAGE = """| Variable | Default | Effect |
| -------- | ------- | ------ |
| `TILO_READ` | off | read by the runtime |
| `TILO_EXPORTED` | off | a launcher hands it on |
| `TILO_SHELL` | off | a script reads it |
| `TILO_PYTHON` | off | a harness reads it |
| `TILO_PASSED_ONLY` | off | a harness hands it on |
"""


class DocumentedFlagTests(unittest.TestCase):
    def test_a_flag_the_runtime_reads_passes(self):
        self.assertEqual(documented({
            "docs/performance-flags.md": "`TILO_READ`\n",
            "crates/ui/src/env.rs": 'let grade = std::env::var("TILO_READ");\n',
        }), [])

    def test_a_flag_only_a_launcher_exports_is_reported(self):
        self.assertEqual(documented({
            "docs/performance-flags.md": "| `TILO_EXPORTED` | off | nothing reads it |\n",
            "dist/session/run": "#!/bin/sh\nexport TILO_EXPORTED=1\n",
        }), ["TILO_EXPORTED"])

    def test_a_flag_a_script_expands_passes(self):
        self.assertEqual(documented({
            "docs/performance-flags.md": "| `TILO_SHELL` | 800x600 | a script reads it |\n",
            "dist/dev/session.sh": 'size=${TILO_SHELL:-800x600}\n',
        }), [])

    def test_a_flag_a_harness_reads_passes_and_one_it_only_passes_on_does_not(self):
        files = {
            "docs/performance-flags.md": PAGE,
            "crates/ui/src/env.rs": 'let read = std::env::var("TILO_READ");\n'
                                    'let size = std::env::var("TILO_HEADLESS_SIZE");\n',
            "dist/dev/session.sh": 'size=${TILO_SHELL:-800x600}\n',
            "scripts/bench.py": 'import os\n'
                                'hz = os.environ.get("TILO_PYTHON", "60")\n'
                                'env = {"TILO_PASSED_ONLY": str(hz)}\n',
        }
        self.assertEqual(documented(files), ["TILO_EXPORTED", "TILO_PASSED_ONLY"])

    def test_the_page_naming_a_flag_is_never_its_reader(self):
        self.assertEqual(documented({
            "docs/performance-flags.md": "`TILO_READ` and again `TILO_READ`\n",
        }), ["TILO_READ"])


if __name__ == "__main__":
    unittest.main()
