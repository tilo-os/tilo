#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest


MODULE_PATH = Path(__file__).parents[1] / "ci_select.py"
SPEC = importlib.util.spec_from_file_location("ci_select", MODULE_PATH)
assert SPEC and SPEC.loader
ci_select = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci_select)


class SelectorTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        def package(name: str, rel: str, deps: list[str] | None = None) -> dict:
            directory = self.root / rel
            return {
                "name": name,
                "manifest_path": str(directory / "Cargo.toml"),
                "targets": [{"name": name}],
                "dependencies": [
                    {"name": dep, "path": str(self.root / dep)} for dep in (deps or [])
                ],
            }
        self.metadata = {"packages": [
            package("base", "base"),
            package("middle", "middle", ["base"]),
            package("leaf", "leaf", ["middle"]),
            package("qdoc", "qdoc"),
            package("pkg", "pkg"),
            package("collab-types", "collab-types"),
        ]}

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_reverse_dependency_closure(self) -> None:
        plan = ci_select.make_plan(self.metadata, self.root, ["base/src/lib.rs"], 2)
        self.assertEqual(set(plan["selected"]), {"base", "middle", "leaf", "qdoc", "pkg", "collab-types"})
        self.assertFalse(plan["full"])

    def test_leaf_change_does_not_select_dependencies(self) -> None:
        plan = ci_select.make_plan(self.metadata, self.root, ["leaf/src/lib.rs"], 2)
        self.assertIn("leaf", plan["selected"])
        self.assertNotIn("base", plan["selected"])

    def test_build_input_forces_full_workspace(self) -> None:
        plan = ci_select.make_plan(self.metadata, self.root, ["Cargo.lock"], 2)
        self.assertTrue(plan["full"])
        self.assertEqual(plan["selected_count"], plan["workspace_count"])

    def test_unowned_deleted_crate_path_forces_full_workspace(self) -> None:
        plan = ci_select.make_plan(self.metadata, self.root, ["apps/removed/Cargo.toml"], 2)
        self.assertTrue(plan["full"])

    def test_balancing_assigns_every_package_once(self) -> None:
        packages, _ = ci_select.package_maps(self.metadata, self.root)
        shards = ci_select.balance(set(packages), packages, 2)
        args = " ".join(shard["packages"] for shard in shards).split()
        names = args[1::2]
        self.assertEqual(sorted(names), sorted(packages))
        self.assertEqual(len(names), len(set(names)))


if __name__ == "__main__":
    unittest.main()
