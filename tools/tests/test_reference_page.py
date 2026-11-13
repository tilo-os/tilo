"""tools/docs/reference/page.py is the one shape every reference page has."""

import importlib.util
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("reference_page", ROOT / "tools/docs/reference/page.py")
assert SPEC is not None and SPEC.loader is not None
page = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = page
SPEC.loader.exec_module(page)


class PageTests(unittest.TestCase):
    def test_a_page_is_one_h1_then_its_sections(self) -> None:
        written = page.page(
            "Tools",
            "What an agent gets.",
            [page.heading(2, "Workspace"), page.code("python", "tilo.ls(path)")],
        )
        self.assertEqual(
            written,
            "# Tools\n\nWhat an agent gets.\n\n## Workspace\n\n"
            "```python fragment\ntilo.ls(path)\n```\n",
        )

    def test_a_second_h1_is_refused(self) -> None:
        with self.assertRaisesRegex(page.ReferenceError, "exactly one H1"):
            page.page("Tools", "What it is.", [page.heading(1, "Other")])

    def test_a_block_without_fragment_is_refused(self) -> None:
        with self.assertRaisesRegex(page.ReferenceError, "not marked fragment"):
            page.check("# T\n\n```python\nx = 1\n```\n")

    def test_a_block_without_a_language_is_refused(self) -> None:
        with self.assertRaisesRegex(page.ReferenceError, "names no language"):
            page.check("# T\n\n```\nx = 1\n```\n")

    def test_a_docstring_block_becomes_a_fragment(self) -> None:
        written = page.prose(
            """
            An example:

            ```ts
            const tilo = new Tilo();
            ```
            """
        )
        self.assertIn("```typescript fragment\nconst tilo = new Tilo();\n```", written)

    def test_a_docstring_block_in_no_language_a_page_fences_is_refused(self) -> None:
        with self.assertRaisesRegex(page.ReferenceError, "no page fences"):
            page.prose("```cobol\nDISPLAY 'x'.\n```")

    def test_an_unclosed_docstring_block_is_refused(self) -> None:
        with self.assertRaisesRegex(page.ReferenceError, "never closes"):
            page.prose("```python\nx = 1\n")

    def test_a_heading_says_one_plain_thing(self) -> None:
        self.assertEqual(page.heading(3, "Workspace.ls"), "### Workspace.ls")
        for markup in ("A `name`", "A [link]", "A *word*", "two\nlines"):
            with self.subTest(markup=markup), self.assertRaises(page.ReferenceError):
                page.heading(2, markup)

    def test_a_table_escapes_pipes_and_flattens_lines(self) -> None:
        written = page.table(("Option", "What it does"), [("path", "a|b\nc")])
        self.assertEqual(
            written,
            "| Option | What it does |\n| --- | --- |\n| path | a\\|b c |",
        )

    def test_a_row_with_the_wrong_number_of_cells_is_refused(self) -> None:
        with self.assertRaisesRegex(page.ReferenceError, "does not match"):
            page.table(("One", "Two"), [("only",)])

    def test_code_a_page_does_not_fence_is_refused(self) -> None:
        with self.assertRaisesRegex(page.ReferenceError, "not a language"):
            page.code("rust", "fn main() {}")

    def test_an_empty_documented_item_is_refused(self) -> None:
        with self.assertRaisesRegex(page.ReferenceError, "no text"):
            page.prose("   \n\n")


if __name__ == "__main__":
    unittest.main()
