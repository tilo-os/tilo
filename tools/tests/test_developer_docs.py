"""tools/docs/developer_docs.py publishes only pages whose links all resolve."""

import contextlib
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("developer_docs", ROOT / "tools/docs/developer_docs.py")
assert SPEC is not None and SPEC.loader is not None
developer_docs = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = developer_docs
SPEC.loader.exec_module(developer_docs)

MANIFEST = """
base = "/docs/developers/"
repository = "https://forge.example/tilo/tilo"

[api]
path = "api/tilo_app/"
sources = ["crates/sdk/tilo-app"]

[[page]]
slug = ""
source = "docs/developers/start.md"
section = "Start"
nav = "Start"
description = "Start here."

[[page]]
slug = "guide"
source = "docs/guide.md"
section = "Guides"
nav = "Guide"
description = "The guide."
"""

GUIDE = """
# The guide

## Other platforms and architectures

## Notes
## Notes
"""


class DeveloperDocsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.write("docs/developers/pages.toml", MANIFEST)
        self.write("docs/guide.md", GUIDE)
        self.write("docs/internal.md", "# Internal\n")
        self.write("docs/shot.png", "")
        (self.root / "crates/sdk/tilo-app").mkdir(parents=True)

    def tearDown(self) -> None:
        self.directory.cleanup()

    def write(self, path: str, text: str) -> None:
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(textwrap.dedent(text).lstrip(), encoding="utf-8")

    def start(self, body: str) -> None:
        self.write("docs/developers/start.md", "# Start\n\n" + textwrap.dedent(body))

    def run_tool(self, *arguments: str) -> int:
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            return developer_docs.main(list(arguments))

    def check(self) -> int:
        return self.run_tool("--root", str(self.root), "check")

    def export(self) -> Path:
        out = self.root / "out"
        status = self.run_tool("--root", str(self.root), "export", "--out", str(out), "--commit", "abc123")
        self.assertEqual(status, 0)
        return out

    def test_links_become_site_repository_and_api_addresses(self) -> None:
        self.start(
            """
            See [the guide](../guide.md#other-platforms-and-architectures), [notes](../guide.md#notes-1),
            [internal](../internal.md#top), [the API](../../crates/sdk/tilo-app) and ![a shot](../shot.png).

            [ref]: ../guide.md
            """
        )
        out = self.export()
        page = (out / "pages/index.md").read_text(encoding="utf-8")
        self.assertIn("(/docs/developers/guide/#other-platforms-and-architectures)", page)
        self.assertIn("(/docs/developers/guide/#notes-1)", page)
        self.assertIn("(https://forge.example/tilo/tilo/src/commit/abc123/docs/internal.md#top)", page)
        self.assertIn("(/docs/developers/api/tilo_app/)", page)
        self.assertIn("(https://forge.example/tilo/tilo/raw/commit/abc123/docs/shot.png)", page)
        self.assertIn("[ref]: /docs/developers/guide/", page)
        self.assertIn('title: "Start"', page)
        manifest = json.loads((out / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual([section["title"] for section in manifest["sections"]], ["Start", "Guides"])
        self.assertEqual(manifest["sections"][1]["pages"][0]["url"], "/docs/developers/guide/")
        self.assertEqual(manifest["api"]["url"], "/docs/developers/api/tilo_app/")

    def test_code_is_left_alone(self) -> None:
        self.start(
            """
            Inline `[x](missing.md)` stays.

            ```markdown
            # Not a heading
            [y](also-missing.md)
            ```
            """
        )
        page = (self.export() / "pages/index.md").read_text(encoding="utf-8")
        self.assertIn("`[x](missing.md)`", page)
        self.assertIn("[y](also-missing.md)", page)

    def test_a_missing_file_fails(self) -> None:
        self.start("[gone](../gone.md)\n")
        self.assertEqual(self.check(), 1)

    def test_a_missing_heading_in_a_published_page_fails(self) -> None:
        self.start("[nowhere](../guide.md#no-such-heading)\n")
        self.assertEqual(self.check(), 1)

    def test_a_missing_heading_on_the_same_page_fails(self) -> None:
        self.start("[nowhere](#no-such-heading)\n")
        self.assertEqual(self.check(), 1)

    def test_a_missing_page_fails(self) -> None:
        self.assertEqual(self.check(), 1)

    def test_a_page_needs_its_title_first(self) -> None:
        self.write("docs/developers/start.md", "## Not a title\n")
        self.assertEqual(self.check(), 1)

    def test_the_published_pages_resolve(self) -> None:
        self.assertEqual(self.run_tool("check"), 0)


if __name__ == "__main__":
    unittest.main()
