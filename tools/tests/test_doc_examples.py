"""tools/docs/doc_examples.py holds the developer docs' examples to their conventions and runs them."""

import contextlib
import importlib.util
import io
from pathlib import Path
import stat
import sys
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("doc_examples", ROOT / "tools/docs/doc_examples.py")
assert SPEC is not None and SPEC.loader is not None
doc_examples = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = doc_examples
SPEC.loader.exec_module(doc_examples)

MANIFEST = """
base = "/docs/"
repository = "https://code.tilo.so/tilo/tilo"

[examples]
languages = ["python", "typescript"]

[[page]]
slug = ""
source = "docs/tilo-sh/index.md"
section = "Get started"
nav = "Overview"
description = "Where to start."

[[page]]
slug = "quickstart"
source = "docs/tilo-sh/quickstart.md"
section = "Get started"
nav = "Quickstart"
description = "Your first workspace."
"""

PYTHON = """
```python example=first-workspace
from tilo import Tilo

print(Tilo)
```
"""

TYPESCRIPT = """
```typescript example=first-workspace
import { Tilo } from "tilo-sdk";

console.log(Tilo);
```
"""

DOOR = """\
#!/bin/sh
# Stands in for door-on-relay: its program runs with the environment it was handed.
shift
exec "$@"
"""


class DocExampleTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.book(MANIFEST)
        self.page(PYTHON + TYPESCRIPT)

    def tearDown(self) -> None:
        self.directory.cleanup()

    def write(self, path: str, text: str) -> Path:
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(textwrap.dedent(text).lstrip(), encoding="utf-8")
        return target

    def executable(self, path: str, text: str) -> Path:
        target = self.write(path, text)
        target.chmod(target.stat().st_mode | stat.S_IXUSR)
        return target

    def book(self, manifest: str) -> None:
        """Write the book's manifest, and the page every test but its own leaves empty."""
        self.write("docs/tilo-sh/pages.toml", manifest)
        self.write("docs/tilo-sh/quickstart.md", "# Quickstart\n")

    def page(self, *parts: str) -> None:
        """Write the book's first page: its title, then the blocks the parts hold."""
        self.write("docs/tilo-sh/index.md", "# Docs\n" + "".join(textwrap.dedent(part) for part in parts))

    def run_tool(self, *arguments: str) -> tuple[int, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            status = doc_examples.main(["--root", str(self.root), *arguments])
        return status, out.getvalue() + err.getvalue()

    def check(self) -> tuple[int, str]:
        return self.run_tool("check")

    def load(self) -> "doc_examples.Book":
        return doc_examples.load(self.root)

    def shell_book(self, *parts: str) -> tuple[Path, Path]:
        """A book whose examples are shell, with a door and a tilo command of its own."""
        self.book(MANIFEST.replace('["python", "typescript"]', '["sh"]'))
        self.page(*parts)
        return self.executable("door", DOOR), self.executable("bin/tilo", "#!/bin/sh\necho \"tilo $*\"\n")

    def test_the_fixture_page_follows_the_conventions(self) -> None:
        book = doc_examples.load(ROOT, "tools/tests/fixtures/docs-examples/pages.toml")
        self.assertEqual(book.problems, ())
        self.assertEqual([example.id for example in book.examples], ["create-a-workspace"])
        self.assertEqual(book.examples[0].languages, ("python", "typescript", "sh"))

    def test_a_generated_page_is_left_to_its_generator(self) -> None:
        self.book(MANIFEST + """
            [[page]]
            slug = "python-sdk"
            source = "sdk/tilo/__init__.py"
            generator = "tools/reference.py"
            section = "Reference"
            nav = "Python SDK"
            description = "Generated."
        """)
        self.write("sdk/tilo/__init__.py", '"""```python\nnot a page\n```"""\n')
        status, output = self.check()
        self.assertEqual(status, 0, output)

    def test_consecutive_blocks_are_one_example(self) -> None:
        book = self.load()
        self.assertEqual(book.problems, ())
        self.assertEqual([example.id for example in book.examples], ["first-workspace"])
        self.assertEqual(book.examples[0].languages, ("python", "typescript"))
        self.assertEqual(len(book.blocks), 2)
        status, output = self.check()
        self.assertEqual(status, 0, output)
        self.assertIn("1 examples, 2 blocks", output)

    def test_a_block_names_its_place_in_the_page(self) -> None:
        python, typescript = self.load().examples[0].blocks
        self.assertEqual(python.location, "docs/tilo-sh/index.md:3")
        self.assertEqual(typescript.location, "docs/tilo-sh/index.md:9")
        self.assertEqual(typescript.lang, "typescript")
        self.assertEqual(typescript.code, 'import { Tilo } from "tilo-sdk";\n\nconsole.log(Tilo);')

    def test_list_names_every_example(self) -> None:
        status, output = self.run_tool("list")
        self.assertEqual(status, 0, output)
        self.assertIn("first-workspace  docs/tilo-sh/index.md:3  python, typescript", output)

    def test_an_id_is_kebab_case(self) -> None:
        self.page((PYTHON + TYPESCRIPT).replace("first-workspace", "First_Workspace"))
        status, output = self.check()
        self.assertEqual(status, 1)
        self.assertIn("is not lowercase kebab case", output)

    def test_every_example_has_every_language(self) -> None:
        self.page(PYTHON)
        status, output = self.check()
        self.assertEqual(status, 1)
        self.assertIn("example 'first-workspace' has no typescript block", output)

    def test_an_id_that_comes_back_starts_a_new_group(self) -> None:
        self.page(PYTHON, TYPESCRIPT, """
            ```json
            {"workspace": "jane-doe"}
            ```
            """, PYTHON)
        status, output = self.check()
        self.assertEqual(status, 1)
        self.assertIn("starts again here", output)

    def test_a_repeated_language_is_a_problem(self) -> None:
        self.page(PYTHON + TYPESCRIPT + PYTHON.replace("print(Tilo)", "print(Tilo.workspaces)"))
        status, output = self.check()
        self.assertEqual(status, 1)
        self.assertIn("has two python blocks", output)

    def test_an_id_belongs_to_one_example_across_the_pages(self) -> None:
        self.write("docs/tilo-sh/quickstart.md", "# Quickstart\n" + PYTHON + TYPESCRIPT)
        status, output = self.check()
        self.assertEqual(status, 1)
        self.assertIn("starts again here", output)
        self.assertIn("docs/tilo-sh/index.md:", output)

    def test_an_unknown_language_is_a_problem(self) -> None:
        self.page(PYTHON + TYPESCRIPT.replace("typescript", "rust"))
        status, output = self.check()
        self.assertEqual(status, 1)
        self.assertIn("unknown language 'rust'", output)

    def test_a_program_without_an_example_is_a_problem(self) -> None:
        self.page(PYTHON, TYPESCRIPT, """
            ```python
            from tilo import Tilo
            ```
            """)
        status, output = self.check()
        self.assertEqual(status, 1)
        self.assertIn("needs example=<id>", output)
        self.assertIn("mark it fragment", output)

    def test_a_fragment_is_not_run(self) -> None:
        self.page(PYTHON, TYPESCRIPT, """
            ```python fragment
            await ws.do("notes/Todo#append-paragraph", text="Buy milk")
            ```
            """)
        status, output = self.check()
        self.assertEqual(status, 0, output)

    def test_a_block_is_an_example_or_a_fragment(self) -> None:
        self.page(PYTHON, TYPESCRIPT, """
            ```python example=first-workspace fragment
            await ws.do("notes/Todo#append-paragraph", text="Buy milk")
            ```
            """)
        status, output = self.check()
        self.assertEqual(status, 1)
        self.assertIn("an example or a fragment, not both", output)

    def test_a_shell_block_may_stand_alone(self) -> None:
        self.page(PYTHON, TYPESCRIPT, """
            ```sh
            tilo account workspaces ls
            ```
            """)
        status, output = self.check()
        self.assertEqual(status, 0, output)

    def test_other_languages_are_left_alone(self) -> None:
        self.page(PYTHON, TYPESCRIPT, """
            ```json
            {"workspace": "jane-doe"}
            ```

            ```toml
            [examples]
            ```
            """)
        status, output = self.check()
        self.assertEqual(status, 0, output)

    def test_an_example_needs_an_id(self) -> None:
        self.page(PYTHON, TYPESCRIPT, """
            ```python example=
            from tilo import Tilo
            ```
            """)
        status, output = self.check()
        self.assertEqual(status, 1)
        self.assertIn("example= names no id", output)

    def test_the_language_comes_first(self) -> None:
        self.page(PYTHON, TYPESCRIPT, """
            ```example=first-workspace python
            from tilo import Tilo
            ```
            """)
        status, output = self.check()
        self.assertEqual(status, 1)
        self.assertIn("a block names its language first", output)

    def test_a_missing_page_is_a_problem(self) -> None:
        (self.root / "docs/tilo-sh/index.md").unlink()
        status, output = self.check()
        self.assertEqual(status, 1)
        self.assertIn("docs/tilo-sh/index.md: does not exist", output)

    def test_run_passes_and_fails_each_block(self) -> None:
        door, tilo = self.shell_book("""
            ```sh example=listing
            tilo account workspaces ls
            ```

            ```sh example=broken
            echo "no such workspace" >&2
            exit 3
            ```
            """)
        status, output = self.run_tool("run", "--door", str(door), "--tilo", str(tilo))
        self.assertEqual(status, 1, output)
        self.assertIn("ok   listing [sh] docs/tilo-sh/index.md:3", output)
        self.assertIn("FAIL broken [sh] docs/tilo-sh/index.md:7", output)
        self.assertIn("no such workspace", output)
        self.assertIn("docs examples: 1 passed, 1 failed", output)

    def test_run_only_runs_the_named_example(self) -> None:
        door, tilo = self.shell_book("""
            ```sh example=listing
            tilo account workspaces ls
            ```

            ```sh example=broken
            exit 3
            ```
            """)
        status, output = self.run_tool("run", "--door", str(door), "--tilo", str(tilo), "--only", "listing")
        self.assertEqual(status, 0, output)
        self.assertEqual(output.count("[sh]"), 1, output)

    def test_run_names_an_unknown_example(self) -> None:
        door, tilo = self.shell_book("""
            ```sh example=listing
            tilo account workspaces ls
            ```
            """)
        status, output = self.run_tool("run", "--door", str(door), "--tilo", str(tilo), "--only", "missing")
        self.assertEqual(status, 1)
        self.assertIn("--only missing: no such example", output)

    def test_run_says_what_a_shell_example_needs(self) -> None:
        door, _ = self.shell_book("""
            ```sh example=listing
            tilo account workspaces ls
            ```
            """)
        status, output = self.run_tool("run", "--door", str(door))
        self.assertEqual(status, 1)
        self.assertIn("a sh example runs the tilo command", output)

    def test_run_says_curl_examples_cannot_run_yet(self) -> None:
        self.book(MANIFEST.replace('["python", "typescript"]', '["curl"]'))
        self.page("""
            ```curl example=listing
            curl "$TILO_DOOR/mcp"
            ```
            """)
        status, output = self.run_tool("run", "--door", str(self.executable("door", DOOR)))
        self.assertEqual(status, 1, output)
        self.assertIn("curl examples cannot run yet", output)

    def test_run_fails_on_the_conventions_before_running_anything(self) -> None:
        door, tilo = self.shell_book("""
            ```sh example=Listing
            exit 0
            ```
            """)
        status, output = self.run_tool("run", "--door", str(door), "--tilo", str(tilo))
        self.assertEqual(status, 1)
        self.assertIn("is not lowercase kebab case", output)
        self.assertNotIn("[sh]", output)

    def test_run_needs_a_door(self) -> None:
        door, tilo = self.shell_book("""
            ```sh example=listing
            exit 0
            ```
            """)
        status, output = self.run_tool("run", "--door", str(self.root / "no-door"), "--tilo", str(tilo))
        self.assertEqual(status, 1)
        self.assertIn("no door to serve the examples with", output)


if __name__ == "__main__":
    unittest.main()
