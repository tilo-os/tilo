#!/usr/bin/env python3
"""The unified workspace keeps every architecture's lines, features and all.

A write resolves one architecture's features only, so it keeps the other architecture's feature
lists. What a package is, its path or its version, is the workspace's whichever architecture
builds it, and cargo refuses a manifest that names one differently for each: the write therefore
reconciles the other architecture's lines to this one's, and `--check` fails, on any architecture
and without resolving anything, while such a table still names a version Cargo.lock does not.
"""
from __future__ import annotations

import contextlib
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).parents[1] / "ci" / "workspace_unify.py"
SPEC = importlib.util.spec_from_file_location("workspace_unify", MODULE_PATH)
assert SPEC and SPEC.loader
unify = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(unify)

LOCK = """\
version = 4

[[package]]
name = "accesskit-unix"
version = "0.23.0"

[[package]]
name = "arm-only"
version = "1.5.0"

[[package]]
name = "hickory-proto"
version = "0.26.2"

[[package]]
name = "member"
version = "0.1.0"

[[package]]
name = "serde"
version = "1.0.219"

[[package]]
name = "serde"
version = "2.0.0"
"""


def line(key: str, source: str, features: list[str]) -> str:
    """A dependency line as the tool writes one."""
    listed = ", ".join(json.dumps(feature) for feature in features)
    return f'{json.dumps(key)} = {{ {source}default-features = false, features = [{listed}] }}\n'


MEMBER = line("member", 'path = "../../../crates/collab/member", ', ["arm"])
PATCHED = line("accesskit-unix-0_23_0", 'package = "accesskit-unix", path = "../../../vendor/accesskit_unix", ', ["tokio"])
HICKORY = line("hickory-proto-0_26_1", 'package = "hickory-proto", version = "^0.26.1", ', ["access-control", "std"])
ARM_ONLY = line("arm-only-1_4_0", 'package = "arm-only", version = "^1.4.0", ', ["std"])
SERDE = line("serde-2_0_0", 'package = "serde", version = "^2.0.0", ', [])
GONE = line("gone-1_0_0", 'package = "gone", version = "^1.0.0", ', [])

# This machine's resolution of the same packages.
MEMBER_HERE = line("member", 'path = "../../../crates/collab/member", ', ["here"])
PATCHED_HERE = line("accesskit-unix-0_23_0", 'package = "accesskit-unix", path = "../../../vendor/accesskit_unix-new", ', ["x11"])
HICKORY_HERE = line("hickory-proto-0_26_2", 'package = "hickory-proto", version = "^0.26.2", ', ["access-control", "std", "tokio"])
HICKORY_MOVED = line("hickory-proto-0_26_2", 'package = "hickory-proto", version = "^0.26.2", ', ["access-control", "std"])
ARM_ONLY_MOVED = line("arm-only-1_5_0", 'package = "arm-only", version = "^1.5.0", ', ["std"])
SERDE_HERE = line("serde-1_0_219", 'package = "serde", version = "^1.0.219", ', [])


class WorkspaceUnifyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.manifest = self.root / "crates" / "dev" / "workspace-unify" / "Cargo.toml"
        for patcher in (
            mock.patch.object(unify, "ROOT", self.root),
            mock.patch.object(unify, "DIR", self.manifest.parent),
            mock.patch.object(unify, "ARCH", "x86_64"),
        ):
            patcher.start()
            self.addCleanup(patcher.stop)
        (self.root / "Cargo.lock").write_text(LOCK)
        self.built = {"member": MEMBER_HERE, "accesskit-unix-0_23_0": PATCHED_HERE,
                      "hickory-proto-0_26_2": HICKORY_HERE, "serde-1_0_219": SERDE_HERE}

    def tables(self, arm: str) -> str:
        """A manifest this tool wrote, its x86_64 tables from the last write there and its aarch64
        tables from the last write on a machine of that architecture."""
        text = unify.HEADER + unify.EMPTY.replace("[features]", 'build = "build.rs"\n\n[features]', 1)
        for arch, lines in (("x86_64", "".join(self.built[key] for key in sorted(self.built))),
                            ("aarch64", arm)):
            for kind in ("dependencies", "build-dependencies"):
                text += f"\n{unify.header(arch, kind)}\n{lines if kind == 'dependencies' else ''}"
        return text

    def write(self, arm: str) -> str:
        with mock.patch.object(unify, "resolved", return_value=(self.built, {})), \
                mock.patch.object(unify, "seeded", return_value=unify.EMPTY):
            return unify.manifest(self.root, self.tables(arm))

    def test_a_write_takes_this_architectures_versions_and_sources_and_keeps_its_features(self) -> None:
        arm = unify.sections(self.write(MEMBER + PATCHED + HICKORY + GONE + ARM_ONLY + SERDE))[
            unify.header("aarch64", "dependencies")]
        self.assertIn(unify.with_features(MEMBER_HERE, '["arm"]'), arm)
        self.assertIn(unify.with_features(PATCHED_HERE, '["tokio"]'), arm)
        self.assertIn(HICKORY_MOVED, arm,
                      "a crate version the lock moved keeps its line, at the version the lock has")
        self.assertIn(ARM_ONLY_MOVED, arm,
                      "a crate this architecture does not build takes the version too")
        self.assertIn(SERDE, arm, "a crate the lock resolves at several versions resolves apart")
        self.assertNotIn(GONE, arm, "a package the workspace no longer has keeps cargo from loading it")

    def test_check_names_the_architecture_whose_table_the_lock_moved(self) -> None:
        problems = unify.stale(self.root, self.tables(MEMBER + PATCHED + HICKORY + GONE + ARM_ONLY + SERDE))
        self.assertEqual(problems, [
            ("aarch64", "names hickory-proto 0.26.1, which Cargo.lock no longer resolves (0.26.2)"),
            ("aarch64", "names gone, which the workspace no longer has"),
            ("aarch64", "names arm-only 1.4.0, which Cargo.lock no longer resolves (1.5.0)"),
        ])

    def test_a_table_consistent_with_the_lock_passes_the_check(self) -> None:
        arm = unify.with_features(HICKORY_HERE, '["access-control", "std"]') + ARM_ONLY_MOVED
        self.assertEqual(unify.stale(self.root, self.tables(MEMBER + PATCHED + arm + SERDE)), [])

    def test_check_fails_before_it_resolves_anything(self) -> None:
        self.manifest.parent.mkdir(parents=True, exist_ok=True)
        self.manifest.write_text(self.tables(HICKORY))
        with mock.patch.object(unify, "resolved") as resolved, \
                mock.patch.object(sys, "argv", ["workspace_unify.py", "--check"]), \
                contextlib.redirect_stderr(stderr := io.StringIO()):
            self.assertEqual(unify.main(), 1)
        self.assertIn("aarch64 table names hickory-proto 0.26.1", stderr.getvalue())
        resolved.assert_not_called()

    def test_a_write_still_writes_this_architecture_so_the_two_converge(self) -> None:
        self.manifest.parent.mkdir(parents=True, exist_ok=True)
        self.manifest.write_text(self.tables(HICKORY))
        with mock.patch.object(unify, "resolved", return_value=(self.built, {})), \
                mock.patch.object(unify, "seeded", return_value=unify.EMPTY), \
                mock.patch.object(unify, "members", return_value=set()), \
                mock.patch.object(unify.subprocess, "run"), \
                mock.patch.object(sys, "argv", ["workspace_unify.py", "--write"]):
            self.assertEqual(unify.main(), 0)
        self.assertIn(unify.with_features(HICKORY_HERE, '["access-control", "std"]'),
                      self.manifest.read_text())


if __name__ == "__main__":
    unittest.main()
