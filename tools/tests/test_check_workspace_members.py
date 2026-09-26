#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest


MODULE_PATH = Path(__file__).parents[1] / "policy/check_workspace_members.py"
SPEC = importlib.util.spec_from_file_location("check_workspace_members", MODULE_PATH)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)


class WorkspaceMemberTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def write(self, relative: str, contents: str) -> None:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents, encoding="utf-8")

    def configure(self, members: list[str], exclude: list[str] | None = None) -> None:
        member_rows = "\n".join(f'    "{member}",' for member in members)
        exclude_rows = "\n".join(f'    "{path}",' for path in (exclude or []))
        self.write(
            "Cargo.toml",
            f"[workspace]\nmembers = [\n{member_rows}\n]\nexclude = [\n{exclude_rows}\n]\n",
        )

    def crate(self, relative: str) -> None:
        self.write(f"{relative}/Cargo.toml", '[package]\nname = "fixture"\nversion = "0.1.0"\n')

    def errors(self) -> list[str]:
        state = checker.inventory(self.root)
        return checker.violations(self.root, state)

    def test_workspace_member_inventory_is_complete_unique_and_consistent(self) -> None:
        with self.subTest(phase='complete inventory accepts root and discovered members'):
            self.doCleanups()
            self.setUp()
            self.crate("apps/editor")
            self.crate("crates/media/core")
            self.crate("examples/packaging/hello")
            self.crate("fs")
            self.configure(
                ["apps/editor", "crates/media/core", "examples/packaging/hello", "fs"]
            )
            self.assertEqual(self.errors(), [])
        with self.subTest(phase='duplicate member is rejected'):
            self.doCleanups()
            self.setUp()
            self.crate("apps/editor")
            self.configure(["apps/editor", "apps/editor"])
            self.assertEqual(
                self.errors(),
                ["duplicate workspace member (2 entries): apps/editor"],
            )
        with self.subTest(phase='unlisted and stale members are both reported'):
            self.doCleanups()
            self.setUp()
            self.crate("crates/media/core")
            self.crate("examples/packaging/hello")
            self.configure(["crates/missing"])
            self.assertEqual(
                self.errors(),
                [
                    "crate is not a workspace member: crates/media/core",
                    "crate is not a workspace member: examples/packaging/hello",
                    "workspace member has no Cargo.toml: crates/missing",
                ],
            )
        with self.subTest(phase='excluded crates and nested workspaces are not required'):
            self.doCleanups()
            self.setUp()
            self.crate("apps/editor")
            self.crate("crates/wasm")
            self.write("crates/fixtures/demo/Cargo.toml", "[workspace]\nmembers = []\n")
            self.configure(["apps/editor"], exclude=["crates/wasm"])
            self.assertEqual(self.errors(), [])
        with self.subTest(phase='member cannot also be excluded'):
            self.doCleanups()
            self.setUp()
            self.crate("crates/wasm")
            self.configure(["crates/wasm"], exclude=["crates/wasm"])
            self.assertEqual(
                self.errors(),
                ["path is both a workspace member and excluded: crates/wasm"],
            )


if __name__ == "__main__":
    unittest.main()
