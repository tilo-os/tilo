#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest


MODULE_PATH = Path(__file__).parents[1] / "ci/select.py"
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
        self.assertEqual(set(plan["tested"]), {"base", "qdoc", "pkg", "collab-types"})
        self.assertEqual(len(plan["matrix"]["include"]), 1)
        row = plan["matrix"]["include"][0]
        self.assertEqual(row["arch"], "x86_64")
        self.assertEqual(set(row["test_packages"].split()[1::2]), set(plan["tested"]))

    def test_leaf_change_does_not_select_dependencies(self) -> None:
        plan = ci_select.make_plan(self.metadata, self.root, ["leaf/src/lib.rs"], 2)
        self.assertIn("leaf", plan["selected"])
        self.assertNotIn("base", plan["selected"])
        self.assertEqual(plan["shard_count"], 1)

    def test_design_only_has_explicit_empty_rust_matrix(self) -> None:
        plan = ci_select.make_plan(self.metadata, self.root, ["docs/designs/ci.md"], 2)
        self.assertFalse(plan["rust_required"])
        self.assertEqual(plan["selected_count"], 0)
        self.assertEqual(plan["shard_count"], 0)
        self.assertEqual(plan["matrix"], {"include": []})
        output = self.root / "outputs"
        ci_select.emit_github(plan, output)
        values = dict(line.split("=", 1) for line in output.read_text().splitlines())
        self.assertEqual(values["rust_required"], "false")
        self.assertEqual(values["selected_count"], "0")

    def test_design_exception_never_overrides_code_or_manual_full(self) -> None:
        for changes, full in [
            (["docs/designs/ci.md"], True),
            (["docs/designs/ci.md", "leaf/src/lib.rs"], False),
            (["docs/designs/ci.md", "Cargo.lock"], False),
        ]:
            with self.subTest(changes=changes, full=full):
                plan = ci_select.make_plan(self.metadata, self.root, changes, 2, force_full=full)
                self.assertTrue(plan["rust_required"])
                self.assertIn("leaf", plan["selected"])

    def test_unknown_docs_and_embedded_code_retain_contract_checks(self) -> None:
        for path in ["docs/contracts.md", "docs/packaging.md", "docs/app-development.md",
                     "crates/sdk/ui-core/CHARTS.md", "docs/unknown.md", "docs/designs/fixture.rs",
                     "docs/designs/../contract.md", "unknown-input"]:
            with self.subTest(path=path):
                plan = ci_select.make_plan(self.metadata, self.root, [path], 2)
                self.assertTrue(plan["rust_required"])
                self.assertTrue(ci_select.ALWAYS_PACKAGES.issubset(plan["selected"]))

    def test_audited_developer_docs_omit_only_rust(self) -> None:
        paths = ["AGENTS.md", "DEVELOPMENT.md", "docs/ci-performance.md", "docs/ci-emergency-relief.md"]
        for changes in [[path] for path in paths] + [paths + ["docs/designs/ci.md"]]:
            with self.subTest(changes=changes):
                plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
                self.assertFalse(plan["rust_required"])
                self.assertEqual(plan["selected_count"], 0)
                self.assertEqual(plan["matrix"], {"include": []})

    def test_developer_docs_do_not_mask_mixed_changes_or_full_override(self) -> None:
        for doc in ["AGENTS.md", "DEVELOPMENT.md", "docs/ci-performance.md", "docs/ci-emergency-relief.md"]:
            for extra, force in [(None, True), ("leaf/src/lib.rs", False),
                                 ("Cargo.lock", False), ("docs/packaging.md", False)]:
                with self.subTest(doc=doc, extra=extra, force=force):
                    changes = [doc] + ([extra] if extra else [])
                    plan = ci_select.make_plan(self.metadata, self.root, changes, 2, force_full=force)
                    self.assertTrue(plan["rust_required"])
                    if force or extra == "Cargo.lock":
                        self.assertTrue(plan["full"])
                        self.assertEqual(plan["selected_count"], plan["workspace_count"])
                    if extra == "leaf/src/lib.rs":
                        self.assertIn("leaf", plan["selected"])

    def test_build_input_forces_full_workspace(self) -> None:
        plan = ci_select.make_plan(self.metadata, self.root, ["Cargo.lock"], 2)
        self.assertTrue(plan["full"])
        self.assertEqual(plan["selected_count"], plan["workspace_count"])

    def test_explicit_full_selects_every_package_even_without_changes(self) -> None:
        for changes in ([], ["docs/development-signing.md"]):
            with self.subTest(changes=changes):
                plan = ci_select.make_plan(self.metadata, self.root, changes, 2, force_full=True)
                self.assertTrue(plan["full"])
                self.assertEqual(plan["selected_count"], plan["workspace_count"])
                for arch in ci_select.NATIVE_RUNNERS:
                    rows = [row for row in plan["matrix"]["include"] if row["arch"] == arch]
                    selected = " ".join(row["packages"] for row in rows).split()[1::2]
                    self.assertEqual(sorted(selected), plan["selected"])

    def test_ci_scripts_do_not_force_unrelated_rust_rebuilds(self) -> None:
        plan = ci_select.make_plan(
            self.metadata,
            self.root,
            [
                ".forgejo/workflows/check.yml",
                "os/tests/imager_ephemeral_runner.sh",
                "scripts/check-install-contract.py",
                "tools/ci/runners/imager-ephemeral-linux.sh",
            ],
            2,
        )
        self.assertFalse(plan["full"])
        self.assertEqual(set(plan["selected"]), {"qdoc", "pkg", "collab-types"})
        self.assertEqual(plan["shard_count"], 1)

    def install_quality_helpers(self) -> None:
        for source, test in ci_select.QUALITY_ONLY_HELPERS.items():
            for path in (source, test):
                target = self.root / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text("# fixture for path existence\n")

    def test_tested_ci_helper_changes_have_no_native_shards(self) -> None:
        self.install_quality_helpers()
        for source, test in ci_select.QUALITY_ONLY_HELPERS.items():
            for changes in ([source], [test], [source, test, "docs/ci-emergency-relief.md"]):
                with self.subTest(changes=changes):
                    plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
                    self.assertFalse(plan["rust_required"])
                    self.assertEqual(plan["matrix"], {"include": []})
                    self.assertIn("mandatory repository-policy tests", plan["reason"])

    def test_ci_helper_exception_preserves_unknown_mixed_and_full_checks(self) -> None:
        self.install_quality_helpers()
        for extra in ("tools/unknown.py", ".forgejo/workflows/check.yml",
                      "tools/run-repository-policy.sh", "os/tests/security-boundaries.sh",
                      "leaf/src/lib.rs", "Cargo.lock", "tools/../tools/ci/select.py"):
            with self.subTest(extra=extra):
                plan = ci_select.make_plan(self.metadata, self.root, ["tools/ci/select.py", extra], 2)
                self.assertTrue(plan["rust_required"])
        full = ci_select.make_plan(self.metadata, self.root, ["tools/ci/select.py"], 2, force_full=True)
        self.assertTrue(full["full"])
        self.assertEqual(full["selected_count"], full["workspace_count"])
        self.assertTrue(ci_select.make_plan(self.metadata, self.root, [], 2)["rust_required"])

    def test_missing_helper_or_test_retains_native_checks(self) -> None:
        for source, test in ci_select.QUALITY_ONLY_HELPERS.items():
            for missing in (source, test):
                self.install_quality_helpers()
                (self.root / missing).unlink()
                with self.subTest(source=source, missing=missing):
                    plan = ci_select.make_plan(self.metadata, self.root, [source, test], 2)
                    self.assertTrue(plan["rust_required"])

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

    def test_each_selected_package_runs_once_on_each_native_architecture(self) -> None:
        plan = ci_select.make_plan(self.metadata, self.root, ["Cargo.lock"], 2, force_full=True)
        rows = plan["matrix"]["include"]
        self.assertEqual({row["arch"] for row in rows}, {"x86_64", "aarch64"})
        for arch, runner in ci_select.NATIVE_RUNNERS.items():
            native = [row for row in rows if row["arch"] == arch]
            self.assertEqual({row["runner"] for row in native}, {runner})
            names = " ".join(row["packages"] for row in native).split()[1::2]
            self.assertEqual(sorted(names), plan["selected"])
            self.assertEqual(len(native), plan["shard_count"])
            tested = " ".join(row["test_packages"] for row in native).split()[1::2]
            self.assertEqual(sorted(tested), plan["selected"])

    def test_dependency_and_unknown_inputs_keep_full_x86_feedback(self) -> None:
        for changes in [["Cargo.lock"], ["apps/deleted/src/lib.rs"]]:
            with self.subTest(changes=changes):
                plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
                self.assertTrue(plan["full"])
                self.assertEqual(plan["tested"], plan["selected"])
                self.assertEqual(plan["selected_count"], plan["workspace_count"])
                self.assertEqual([row["arch"] for row in plan["matrix"]["include"]], ["x86_64"])

    def test_unknown_inputs_retain_existing_contract_test_fallback(self) -> None:
        for changes in [["unknown-input"], []]:
            plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
            self.assertEqual(set(plan["tested"]), ci_select.ALWAYS_PACKAGES)
            self.assertTrue(plan["rust_required"])

    def test_package_manifest_change_checks_dependants_and_tests_owner(self) -> None:
        plan = ci_select.make_plan(self.metadata, self.root, ["base/Cargo.toml"], 2)
        self.assertIn("leaf", plan["selected"])
        self.assertIn("base", plan["tested"])
        self.assertNotIn("leaf", plan["tested"])

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

    def test_move_between_packages_selects_both_owners_and_dependants(self) -> None:
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.email", "ci@example.invalid"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.name", "CI"], cwd=self.root, check=True)
        # Identical content encourages Git's normal rename detection. Unicode
        # also exercises its default quoted-path output rather than ASCII only.
        old = "base/src/café.rs"
        new = "leaf/src/café.rs"
        (self.root / old).parent.mkdir(parents=True)
        (self.root / new).parent.mkdir(parents=True)
        (self.root / old).write_text("pub fn moved() {}\n")
        subprocess.run(["git", "add", old], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "before move"], cwd=self.root, check=True)
        base = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()
        subprocess.run(["git", "mv", old, new], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "move between packages"], cwd=self.root, check=True)
        changes = ci_select.changed_files(self.root, base, "HEAD")
        self.assertEqual(set(changes), {old, new})
        plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
        self.assertTrue({"base", "middle", "leaf"}.issubset(plan["selected"]))


if __name__ == "__main__":
    unittest.main()
