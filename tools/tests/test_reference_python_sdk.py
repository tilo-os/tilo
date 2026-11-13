"""tools/docs/reference/python_sdk.py writes the page from the package itself."""

import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
GENERATOR = ROOT / "tools/docs/reference/python_sdk.py"
SDK = ROOT / "sdk/python"


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def write(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8")


class PythonSdkReferenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        shutil.copytree(
            SDK,
            self.root / "sdk/python",
            ignore=shutil.ignore_patterns("__pycache__", ".venv", "node_modules"),
        )
        self.out = self.root / "generated" / "python-sdk.md"

    def tearDown(self) -> None:
        self.directory.cleanup()

    def generate(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, "-I", str(GENERATOR), "--root", str(self.root), "--out", str(self.out)],
            capture_output=True,
            text=True,
            check=False,
        )

    def page(self) -> str:
        return read(self.out)

    def edit(self, relative: str, old: str, new: str) -> None:
        path = self.root / "sdk/python/src/tilo" / relative
        text = read(path)
        self.assertIn(old, text)
        write(path, text.replace(old, new, 1))

    def test_writes_every_export_with_its_signature_and_docstring(self) -> None:
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        page = self.page()
        self.assertTrue(page.startswith("# Python SDK\n"))
        self.assertEqual(page.count("\n# "), 0, "one H1 only")
        for section in ("Client", "Workspaces", "Resources", "Runs", "Files", "Keys", "Members", "Errors"):
            self.assertIn(f"## {section}\n", page)
        self.assertIn("### AsyncTilo\n", page)
        self.assertIn("class AsyncTilo\nAsyncTilo(key: str | None = None", page)
        self.assertIn("#### Workspace.ls\n", page)
        self.assertIn(
            "Workspace.ls(path: str | None = None, *, kind: str | None = None, "
            "limit: int | None = None) -> Any",
            page,
        )
        self.assertIn("The apps, an app's resources, or one resource's children and actions.", page)
        self.assertIn("#### Event.path\n", page)
        self.assertIn("### Refused\n", page)
        self.assertIn("## App types", page)
        self.assertIn("tilo sdk gen --lang python --out tilo/types", page)
        self.assertNotIn("sdk/python/src/tilo/types", page)

    def test_every_block_is_a_fragment_with_a_language(self) -> None:
        self.assertEqual(self.generate().returncode, 0)
        page = self.page()
        language = None
        for line in page.split("\n"):
            if language is None and line.startswith("```"):
                info = line[3:].split()
                self.assertTrue(info, f"a block names no language: {line!r}")
                self.assertIn("fragment", info, f"a block is not a fragment: {line!r}")
                language = info[0]
            elif language is not None and line.startswith("```"):
                language = None

    def test_two_runs_write_the_same_page(self) -> None:
        self.assertEqual(self.generate().returncode, 0)
        first = self.page()
        self.assertEqual(self.generate().returncode, 0)
        self.assertEqual(first, self.page())

    def test_an_undocumented_method_stops_the_page(self) -> None:
        self.edit(
            "_workspace.py",
            '"""The workspace as the account lists it: its id, name, host and your role."""',
            '""""""',
        )
        result = self.generate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("Workspace.info", result.stderr)
        self.assertIn("no docstring", result.stderr)

    def test_an_export_no_group_holds_stops_the_page(self) -> None:
        self.edit("_models.py", "def _text(", 'def extra() -> None:\n    """Something new."""\n\n\ndef _text(')
        self.edit("__init__.py", '    "__version__",', '    "__version__",\n    "extra",')
        result = self.generate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("no group holds: extra", result.stderr)

    def test_a_version_the_manifest_disagrees_with_stops_the_page(self) -> None:
        self.edit("__init__.py", '__version__ = "0.1.0"', '__version__ = "9.9.9"')
        result = self.generate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("9.9.9", result.stderr)

    def test_the_generated_types_package_is_expected(self) -> None:
        shutil.rmtree(self.root / "sdk/python/src/tilo/types")
        result = self.generate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("sdk/python/src/tilo/types is not there", result.stderr)


if __name__ == "__main__":
    unittest.main()
