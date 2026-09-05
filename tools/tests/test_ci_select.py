#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
from pathlib import Path
import subprocess
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
        self.assertEqual(plan["shard_count"], 1)

    def test_build_input_forces_full_workspace(self) -> None:
        plan = ci_select.make_plan(self.metadata, self.root, ["Cargo.lock"], 2)
        self.assertTrue(plan["full"])
        self.assertEqual(plan["selected_count"], plan["workspace_count"])

    def test_ci_scripts_do_not_force_unrelated_rust_rebuilds(self) -> None:
        plan = ci_select.make_plan(
            self.metadata,
            self.root,
            [
                ".forgejo/workflows/check.yml",
                "os/tests/imager_ephemeral_runner.sh",
                "scripts/check-install-contract.py",
                "tools/run-imager-ephemeral-runner.sh",
            ],
            2,
        )
        self.assertFalse(plan["full"])
        self.assertEqual(set(plan["selected"]), {"qdoc", "pkg", "collab-types"})
        self.assertEqual(plan["shard_count"], 1)

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

    def test_cost_threshold_adds_shards_only_when_useful(self) -> None:
        packages, _ = ci_select.package_maps(self.metadata, self.root)
        names = set(packages)
        original = ci_select.SHARD_COST_BUDGET
        try:
            ci_select.SHARD_COST_BUDGET = 1
            self.assertEqual(ci_select.useful_shard_count(names, packages, 2), 2)
            ci_select.SHARD_COST_BUDGET = 10_000
            self.assertEqual(ci_select.useful_shard_count(names, packages, 2), 1)
        finally:
            ci_select.SHARD_COST_BUDGET = original

    def test_github_outputs_include_adaptive_shard_metrics(self) -> None:
        plan = ci_select.make_plan(self.metadata, self.root, ["leaf/src/lib.rs"], 2)
        output = self.root / "github-output"
        ci_select.emit_github(plan, output)
        values = dict(line.split("=", 1) for line in output.read_text().splitlines())
        self.assertEqual(values["shard_count"], "1")
        self.assertEqual(values["estimated_cost"], str(plan["estimated_cost"]))

    def test_changed_files_compares_shallow_base_and_head_trees(self) -> None:
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.email", "ci@example.invalid"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.name", "CI"], cwd=self.root, check=True)
        (self.root / "base.rs").write_text("fn base() {}\n")
        subprocess.run(["git", "add", "base.rs"], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "base"], cwd=self.root, check=True)
        base = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()

        subprocess.run(["git", "checkout", "--orphan", "shallow-head", "-q"], cwd=self.root, check=True)
        subprocess.run(["git", "rm", "-qf", "base.rs"], cwd=self.root, check=True)
        (self.root / "head.rs").write_text("fn head() {}\n")
        subprocess.run(["git", "add", "head.rs"], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "head"], cwd=self.root, check=True)
        head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()

        self.assertEqual(ci_select.changed_files(self.root, base, head), ["base.rs", "head.rs"])


if __name__ == "__main__":
    unittest.main()
