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

    def crate(self, relative: str, edition: str = 'edition = "2024"') -> None:
        self.write(
            f"{relative}/Cargo.toml",
            f'[package]\nname = "fixture"\nversion = "0.1.0"\n{edition}\n',
        )

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
        with self.subTest(phase='every member is on edition 2024'):
            self.doCleanups()
            self.setUp()
            self.crate("apps/current")
            self.crate("apps/inherited", "edition.workspace = true")
            self.crate("apps/older", 'edition = "2021"')
            self.crate("apps/undeclared", "")
            self.configure(["apps/current", "apps/inherited", "apps/older", "apps/undeclared"])
            self.write(
                "Cargo.toml",
                (self.root / "Cargo.toml").read_text(encoding="utf-8")
                + '\n[workspace.package]\nedition = "2024"\n',
            )
            self.assertEqual(
                self.errors(),
                [
                    "workspace member is on edition 2021, not 2024: apps/older",
                    "workspace member is on edition 2015, not 2024: apps/undeclared",
                ],
            )


class StagedWorkspaceMemberTests(unittest.TestCase):
    """The hook judges the manifests a commit stages, not every crate of the tree."""

    def setUp(self) -> None:
        import sys
        sys.path.insert(0, str(Path(__file__).resolve().parent))
        from git_fixture import isolated_git_environment
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.environment = isolated_git_environment()
        self.git("init", "-q")

    def git(self, *args: str) -> None:
        import subprocess
        subprocess.check_output(["git", "-C", str(self.root), *args], env=self.environment)

    def write(self, relative: str, contents: str) -> None:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents, encoding="utf-8")

    def configure(self, members: list[str]) -> None:
        rows = "\n".join(f'    "{member}",' for member in members)
        self.write("Cargo.toml", f"[workspace]\nmembers = [\n{rows}\n]\n")

    def commit(self) -> None:
        self.git("-c", "user.name=Policy Test", "-c", "user.email=policy@example.org",
                 "-c", "commit.gpgsign=false", "commit", "-qm", "the tree")

    def stage(self, relative: str, contents: str) -> None:
        self.write(relative, contents)
        self.git("add", "--", relative)

    def test_a_staged_member_on_the_workspace_edition_passes(self) -> None:
        self.configure(["apps/editor"])
        self.stage("apps/editor/Cargo.toml", '[package]\nname = "editor"\nversion = "0.1.0"\n'
                   'edition = "2024"\n')
        self.commit()
        self.stage("apps/editor/Cargo.toml", '[package]\nname = "editor"\nversion = "0.2.0"\n'
                   'edition = "2024"\n')
        self.assertEqual(checker.staged_violations(self.root), [])

    def test_a_crate_the_commit_stages_must_be_a_member_on_the_edition(self) -> None:
        self.configure(["apps/editor"])
        self.stage("apps/editor/Cargo.toml", '[package]\nname = "editor"\nversion = "0.1.0"\n'
                   'edition = "2024"\n')
        self.commit()
        self.stage("apps/new/Cargo.toml", '[package]\nname = "new"\nversion = "0.1.0"\n'
                   'edition = "2024"\n')
        self.assertEqual(
            checker.staged_violations(self.root),
            ["crate is not a workspace member: apps/new"],
        )
        self.configure(["apps/editor", "apps/new"])
        self.git("add", "--", "Cargo.toml")
        self.stage("apps/new/Cargo.toml", '[package]\nname = "new"\nversion = "0.1.0"\n'
                   'edition = "2021"\n')
        self.assertEqual(
            checker.staged_violations(self.root),
            ["workspace member is on edition 2021, not 2024: apps/new"],
        )

    def test_a_crate_the_commit_does_not_stage_has_its_own_verdict(self) -> None:
        self.configure(["apps/editor"])
        self.stage("apps/editor/Cargo.toml", '[package]\nname = "editor"\nversion = "0.1.0"\n'
                   'edition = "2024"\n')
        self.commit()
        # A crate outside the workspace that the commit leaves alone is the
        # tree-wide audit's finding, and that audit still holds it.
        self.write("apps/undeclared/Cargo.toml", '[package]\nname = "x"\nversion = "0.1.0"\n')
        self.stage("apps/editor/Cargo.toml", '[package]\nname = "editor"\nversion = "0.2.0"\n'
                   'edition = "2024"\n')
        self.assertEqual(checker.staged_violations(self.root), [])


if __name__ == "__main__":
    unittest.main()
