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
            [internal](../internal.md#internal), [the API](../../crates/sdk/tilo-app) and ![a shot](../shot.png).

            [ref]: ../guide.md
            """
        )
        out = self.export()
        page = (out / "pages/index.md").read_text(encoding="utf-8")
        self.assertIn("(/docs/developers/guide/#other-platforms-and-architectures)", page)
        self.assertIn("(/docs/developers/guide/#notes-1)", page)
        self.assertIn("(https://forge.example/tilo/tilo/src/commit/abc123/docs/internal.md#internal)", page)
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

    def second_book(self) -> None:
        self.write("docs/development/pages.toml", """
            base = "/docs/development/"
            repository = "https://forge.example/tilo/tilo"
            related_manifests = ["docs/developers/pages.toml"]
            [[page]]
            slug = ""
            source = "docs/development/index.md"
            section = "Start"
            nav = "Develop Tilo"
            description = "Choose a task."
        """)
        self.write("docs/development/index.md", "# Develop Tilo\n\n[SDK](../developers/start.md)\n")
        self.write("docs/developers/pages.toml",
                   MANIFEST.replace('base = "/docs/developers/"',
                                    'related_manifests = ["docs/development/pages.toml"]\nbase = "/docs/developers/"'))

    def test_books_link_to_each_other_without_source_tree_detours(self) -> None:
        self.second_book()
        self.start("[Development](../development/index.md)\n")
        page = (self.export() / "pages/index.md").read_text()
        self.assertIn("(/docs/development/)", page)
        out = self.root / "development-out"
        self.assertEqual(self.run_tool("--root", str(self.root), "--manifest",
                                      "docs/development/pages.toml", "export",
                                      "--out", str(out)), 0)
        self.assertIn("(/docs/developers/)", (out / "pages/index.md").read_text())
        self.assertIsNone(json.loads((out / "manifest.json").read_text())["api"])

    def test_default_check_checks_the_related_book_too(self) -> None:
        self.second_book()
        self.start("Hello\n")
        self.write("docs/development/index.md", "# Develop Tilo\n\n[Missing](gone.md)\n")
        self.assertEqual(self.check(), 1)

    def test_missing_unpublished_heading_fails(self) -> None:
        self.start("[Internal](../internal.md#gone)\n")
        self.assertEqual(self.check(), 1)

    def test_export_refuses_to_delete_its_sources(self) -> None:
        self.start("Hello\n")
        self.assertEqual(self.run_tool("--root", str(self.root), "export",
                                      "--out", str(self.root / "docs")), 1)
        self.assertTrue((self.root / "docs/developers/start.md").exists())

    def test_export_cannot_delete_the_related_books_sources(self) -> None:
        self.second_book()
        self.start("Hello\n")
        self.assertEqual(self.run_tool("--root", str(self.root), "export",
                                      "--out", str(self.root / "docs/development")), 1)
        self.assertTrue((self.root / "docs/development/index.md").exists())

    def test_export_cannot_overwrite_git_refs(self) -> None:
        import subprocess
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        self.start("Hello\n")
        refs = self.root / ".git/refs/heads"
        sentinel = refs / "keep"
        sentinel.write_text("fixture\n")
        self.assertEqual(self.run_tool("--root", str(self.root), "export",
                                      "--out", str(refs)), 1)
        self.assertEqual(sentinel.read_text(), "fixture\n")

    def test_manifest_cannot_follow_a_symlink_outside_the_source_tree(self) -> None:
        self.start("Hello\n")
        with tempfile.TemporaryDirectory() as directory:
            outside = Path(directory) / "private.md"
            outside.write_text("# Private\n")
            source = self.root / "docs/developers/start.md"
            source.unlink()
            source.symlink_to(outside)
            self.assertEqual(self.check(), 1)

    def test_manifest_cannot_publish_a_file_outside_the_source_tree(self) -> None:
        self.start("Hello\n")
        self.write("docs/developers/pages.toml", MANIFEST.replace(
            'source = "docs/guide.md"', 'source = "../../private.md"'))
        self.assertEqual(self.check(), 1)

    def test_git_tree_export_does_not_require_a_checkout(self) -> None:
        self.start("Hello\n")
        files = {path.relative_to(self.root).as_posix(): path.read_text()
                 for path in self.root.rglob("*.md")}
        files["docs/developers/pages.toml"] = (self.root / "docs/developers/pages.toml").read_text()

        def git(tree, *args):
            if args[0] == "rev-parse":
                return "abc123\n"
            if args[0] == "ls-tree":
                return "\0".join([*files, "crates/sdk/tilo-app/src/lib.rs"]) + "\0"
            if args[0] == "show":
                return files[args[1].split(":", 1)[1]]
            self.fail(f"unexpected Git operation {args}")

        from unittest.mock import patch
        with patch.object(developer_docs.SourceTree, "git", git):
            tree = developer_docs.SourceTree(self.root / "no-checkout", self.root / "bare.git")
            out = self.root / "immutable-out"
            developer_docs.export(self.root / "no-checkout", out, tree.revision,
                                  tree=tree)
        index = json.loads((out / "manifest.json").read_text())
        self.assertEqual(index["commit"], "abc123")
        self.assertIn("/src/commit/abc123/", index["sections"][0]["pages"][0]["sourceUrl"])

    def test_the_published_pages_resolve(self) -> None:
        self.assertEqual(self.run_tool("check"), 0)


if __name__ == "__main__":
    unittest.main()
