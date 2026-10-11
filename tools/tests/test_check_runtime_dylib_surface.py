#!/usr/bin/env python3
"""The runtime surface check reads Cargo's forest the way the release resolves it."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest


MODULE_PATH = Path(__file__).parents[1] / "policy/check_runtime_dylib_surface.py"
SPEC = importlib.util.spec_from_file_location("check_runtime_dylib_surface", MODULE_PATH)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)

TARGET = "x86_64-unknown-linux-gnu"


def forest(*lines: str) -> str:
    return "\n".join(lines) + "\n"


# Two apps and the runtime, deduplicated as `cargo tree --prefix depth` prints
# it: a package's dependencies appear once, under its first appearance.
CLEAN = forest(
    "0notes v0.1.0 (/repo/apps/notes)",
    "1runtime-dylib v0.1.0 (/repo/crates/runtime/runtime-dylib)",
    "2tilo-app v0.1.0 (/repo/crates/sdk/tilo-app)",
    "3tilo-ui v0.1.0 (/repo/crates/sdk/ui-core)",
    "4serde v1.0.228",
    "1tilo-app v0.1.0 (/repo/crates/sdk/tilo-app) (*)",
    "0store v0.1.2 (/repo/apps/store)",
    "1app-host v0.1.0 (/repo/crates/packaging/app-host)",
    "2serde v1.0.228 (*)",
    "1runtime-dylib v0.1.0 (/repo/crates/runtime/runtime-dylib) (*)",
    "1tilo-app v0.1.0 (/repo/crates/sdk/tilo-app) (*)",
    "0runtime-dylib v0.1.0 (/repo/crates/runtime/runtime-dylib) (*)",
)


class RuntimeSurfaceTests(unittest.TestCase):
    def test_an_app_may_link_an_os_crate_the_runtime_does_not_carry(self) -> None:
        self.assertEqual(checker.violations(CLEAN, ["notes", "store"], TARGET), [])

    def test_a_denied_crate_reached_through_a_dependency_fails(self) -> None:
        tree = CLEAN.replace(
            "4serde v1.0.228\n",
            "4serde v1.0.228\n4package-store v0.1.0 (/repo)\n5tilo-fs v0.1.0 (/repo)\n",
        )
        found = checker.violations(tree, ["notes", "store"], TARGET)
        self.assertEqual(len(found), 1)
        self.assertIn("reaches tilo-fs", found[0])

    def test_an_engine_with_its_own_library_back_in_the_runtime_fails(self) -> None:
        # Typst lives in libtilo_typst_engine.so; the workspace-hack
        # carrying its font table into the runtime is a regression even
        # though Typst Editor reaches the crate itself.
        tree = CLEAN.replace(
            "4serde v1.0.228\n",
            "4serde v1.0.228\n2workspace-hack v0.1.0 (/repo)\n3typst-assets v0.15.1\n",
        ).replace(
            "1tilo-app v0.1.0 (/repo/crates/sdk/tilo-app) (*)\n0store",
            "1tilo-app v0.1.0 (/repo/crates/sdk/tilo-app) (*)\n1typst-assets v0.15.1 (*)\n"
            "1workspace-hack v0.1.0 (/repo) (*)\n0store",
        )
        found = checker.violations(tree, ["notes", "store"], TARGET)
        self.assertEqual(len(found), 1, found)
        self.assertIn("reaches typst-assets", found[0])
        self.assertIn("libtilo_typst_engine.so", found[0])

    def test_a_crate_no_app_reaches_fails(self) -> None:
        tree = CLEAN.replace(
            "4serde v1.0.228\n",
            "4serde v1.0.228\n2unused-engine v1.0.0\n",
        )
        found = checker.violations(tree, ["notes", "store"], TARGET)
        self.assertEqual(len(found), 1)
        self.assertIn("carries unused-engine", found[0])

    def test_a_graph_without_the_runtime_or_its_toolkit_fails(self) -> None:
        without_runtime = forest("0notes v0.1.0 (/repo/apps/notes)", "1serde v1.0.228")
        self.assertIn(
            "did not describe runtime-dylib",
            checker.violations(without_runtime, ["notes"], TARGET)[0],
        )
        hollow = forest("0runtime-dylib v0.1.0 (/repo)", "1serde v1.0.228")
        self.assertIn("lacks tilo-app, tilo-ui", checker.violations(hollow, [], TARGET)[0])

    def test_an_app_missing_from_the_graph_fails(self) -> None:
        found = checker.violations(CLEAN, ["notes", "store", "mail"], TARGET)
        self.assertEqual(found, [f"cargo tree did not describe mail for {TARGET}"])


if __name__ == "__main__":
    unittest.main()
