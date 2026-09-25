#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest


MODULE_PATH = Path(__file__).parents[1] / "ci/affected_packages.py"
REPO_ROOT = MODULE_PATH.parents[2]
SPEC = importlib.util.spec_from_file_location("ci_select", MODULE_PATH)
assert SPEC and SPEC.loader
ci_select = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci_select)


class SelectorTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / "Cargo.toml").write_text("[workspace]\n")
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

    def test_hermetic_suites_follow_changed_paths_and_run_all_when_asked(self):
        manifest = ci_select.path_suites.parse(
            '[[group]]\nname = "os"\ninputs = ["os/"]\nsuites = ["os/tests/a.sh"]\n'
            '[[group]]\nname = "perf"\ninputs = ["scripts/perf/"]\nsuites = ["scripts/perf/b_test.py"]\n')
        def plan(changes, **options):
            return ci_select.make_plan(self.metadata, self.root, changes, 2,
                                       suite_manifest=manifest, **options)
        selected = plan(["os/dev/tool.py"])
        self.assertTrue(selected["suites_required"])
        self.assertEqual(selected["suites"], ["os/tests/a.sh"])
        unrelated = plan(["leaf/src/lib.rs"])
        self.assertFalse(unrelated["suites_required"])
        self.assertEqual(unrelated["suites"], [])
        for options in ({"all_suites": True}, {"force_full": True}):
            with self.subTest(options=options):
                self.assertEqual(plan(["leaf/src/lib.rs"], **options)["suites"],
                                 ["os/tests/a.sh", "scripts/perf/b_test.py"])
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "outputs"
            ci_select.emit_github(selected, output)
            lines = output.read_text().splitlines()
        self.assertIn("suites_required=true", lines)
        self.assertIn("suites=os/tests/a.sh", lines)

    def test_cross_package_readers_are_checked_and_tested_when_their_inputs_change(self):
        # "base" reads leaf's sources and a shipped OS file from outside itself,
        # the way tilo-ui's design audit reads every app.
        manifest = ci_select.path_suites.parse(
            '[[cargo]]\npackage = "base"\ninputs = ["leaf/*.rs", "os/usr/share/x.json"]\n'
            'readers = ["base/tests/audit.rs"]\n')
        def plan(changes, **options):
            return ci_select.make_plan(self.metadata, self.root, changes, 2,
                                       suite_manifest=manifest, **options)
        leaf = plan(["leaf/src/main.rs"])
        self.assertEqual(set(leaf["tested"]), {"leaf", "base", "qdoc", "pkg", "collab-types"})
        # The reader's code did not change, so its dependants are not re-checked.
        self.assertNotIn("middle", leaf["selected"])
        self.assertIn("read the changed files", leaf["reason"])
        row = leaf["matrix"]["include"][0]
        self.assertIn("base", row["test_packages"].split()[1::2])
        # A file no package owns selects the package that reads it.
        self.assertIn("base", plan(["os/usr/share/x.json"])["tested"])
        # Paths that no declared reader reads select nothing extra.
        self.assertNotIn("base", plan(["leaf/Cargo.toml"])["tested"])
        self.assertNotIn("base", plan(["os/usr/share/other.json"])["selected"])
        # Without a manifest nothing is attributed.
        self.assertNotIn("base", ci_select.make_plan(
            self.metadata, self.root, ["leaf/src/main.rs"], 2)["tested"])

    def test_a_read_input_is_never_treated_as_quality_only(self):
        (self.root / "DEVELOPMENT.md").write_text("")
        manifest = ci_select.path_suites.parse(
            '[[cargo]]\npackage = "leaf"\ninputs = ["DEVELOPMENT.md"]\nreaders = ["leaf/src/lib.rs"]\n')
        plan = ci_select.make_plan(self.metadata, self.root, ["DEVELOPMENT.md"], 2,
                                   suite_manifest=manifest)
        self.assertTrue(plan["rust_required"])
        self.assertIn("leaf", plan["tested"])

    def test_production_manifest_selects_the_ui_audit_for_an_app_change(self):
        # The regression that motivated reader declarations: an app ported to
        # the resource framework broke tilo-ui's launcher audit, whose tests
        # the dependency graph never selected for an app-only change.
        manifest = ci_select.path_suites.load(REPO_ROOT)
        for path in ("apps/scratchpad/src/main.rs", "apps/scratchpad/src/lib.rs",
                     "apps/scratchpad/tilo-app.toml"):
            with self.subTest(path=path):
                self.assertIn("tilo-ui", ci_select.path_suites.select_packages(manifest, [path]))

    def test_suite_manifest_changes_are_quality_only(self):
        paths = ("tools/ci/suites.py", "tools/ci/suites.toml", "tools/tests/test_suites.py")
        for path in paths:
            (self.root / path).parent.mkdir(parents=True, exist_ok=True)
            (self.root / path).write_text("")
        for path in paths:
            with self.subTest(path=path):
                plan = ci_select.make_plan(self.metadata, self.root, [path], 2,
                                           allow_security_skip=True)
                self.assertFalse(plan["rust_required"])
                self.assertFalse(plan["security_required"])

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

    def test_tilo_ui_test_link_uses_large_x86_runner(self) -> None:
        self.metadata["packages"].append({
            "name": "tilo-ui",
            "manifest_path": str(self.root / "tilo-ui" / "Cargo.toml"),
            "targets": [{"name": "tilo-ui"}],
            "dependencies": [],
        })
        plan = ci_select.make_plan(
            self.metadata, self.root, ["tilo-ui/src/lib.rs"], 2
        )
        self.assertEqual(plan["matrix"]["include"][0]["runner"], "runner-large")
        # Only the test links need the large runner; check and Clippy do not.
        self.assertEqual(plan["matrix"]["include"][0]["check_runner"], "runner")

        ordinary = ci_select.make_plan(
            self.metadata, self.root, ["leaf/src/lib.rs"], 2
        )
        self.assertEqual(ordinary["matrix"]["include"][0]["runner"], "runner")
        self.assertEqual(ordinary["matrix"]["include"][0]["check_runner"], "runner")

    def test_design_only_has_explicit_empty_rust_matrix(self) -> None:
        plan = ci_select.make_plan(self.metadata, self.root, ["docs/designs/ci.md"], 2)
        self.assertFalse(plan["rust_required"])
        self.assertTrue(plan["security_required"])
        self.assertEqual(plan["selected_count"], 0)
        self.assertEqual(plan["shard_count"], 0)
        self.assertEqual(plan["matrix"], {"include": []})
        self.assertEqual(plan["clippy_matrix"], {"include": []})
        output = self.root / "outputs"
        ci_select.emit_github(plan, output)
        values = dict(line.split("=", 1) for line in output.read_text().splitlines())
        self.assertEqual(values["rust_required"], "false")
        self.assertEqual(values["security_required"], "true")
        self.assertEqual(values["selected_count"], "0")

    def test_security_skip_is_limited_to_audited_pull_request_inputs(self) -> None:
        self.install_quality_helpers()
        prose = [
            "AGENTS.md",
            "DEVELOPMENT.md",
            "docs/ci-performance.md",
            "docs/ci-emergency-relief.md",
            "docs/headless-testing.md",
            "docs/designs/ci.md",
        ]
        for path in prose:
            target = self.root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text("audited prose\n")
        for path in prose:
            with self.subTest(path=path):
                plan = ci_select.make_plan(
                    self.metadata, self.root, [path], 2, allow_security_skip=True)
                self.assertFalse(plan["security_required"])
        for source, test in ci_select.QUALITY_ONLY_HELPERS.items():
            for changes in ([source], [test], [source, test, "docs/designs/ci.md"]):
                with self.subTest(changes=changes):
                    plan = ci_select.make_plan(
                        self.metadata, self.root, changes, 2, allow_security_skip=True)
                    self.assertFalse(plan["security_required"])

    def test_security_selector_defaults_true_for_non_pr_and_full_runs(self) -> None:
        for changes, options in [
            (["docs/designs/ci.md"], {}),
            (["docs/designs/ci.md"], {"force_full": True}),
            (["tools/ci/affected_packages.py"], {}),
        ]:
            with self.subTest(changes=changes, options=options):
                self.install_quality_helpers()
                plan = ci_select.make_plan(self.metadata, self.root, changes, 2, **options)
                self.assertTrue(plan["security_required"])

    def test_security_selector_rejects_control_sensitive_and_mixed_inputs(self) -> None:
        self.install_quality_helpers()
        self.install_control_inputs()
        excluded = [
            ".forgejo/workflows/check.yml",
            "os/tests/security-boundaries.sh",
            "tools/run-security-boundary-invariants.sh",
            "Cargo.toml",
            "os/external-assets/github-assets.tsv",
            "SECURITY.md",
            "tools/unknown.py",
        ]
        for path in excluded:
            with self.subTest(path=path):
                plan = ci_select.make_plan(
                    self.metadata, self.root, [path], 2, allow_security_skip=True)
                self.assertTrue(plan["security_required"])
        mixed = ci_select.make_plan(
            self.metadata,
            self.root,
            ["tools/ci/affected_packages.py", "SECURITY.md"],
            2,
            allow_security_skip=True,
        )
        self.assertTrue(mixed["security_required"])

    def test_security_selector_fails_closed_for_deleted_or_unobserved_helpers(self) -> None:
        deleted_prose = self.root / "docs/designs/deleted.md"
        deleted_prose.parent.mkdir(parents=True, exist_ok=True)
        deleted_prose.write_text("audited prose\n")
        deleted_prose.unlink()
        self.assertTrue(ci_select.make_plan(
            self.metadata,
            self.root,
            ["docs/designs/deleted.md"],
            2,
            allow_security_skip=True,
        )["security_required"])
        for source, test in ci_select.QUALITY_ONLY_HELPERS.items():
            for missing in (source, test):
                self.install_quality_helpers()
                (self.root / missing).unlink()
                with self.subTest(source=source, missing=missing):
                    plan = ci_select.make_plan(
                        self.metadata,
                        self.root,
                        [source],
                        2,
                        allow_security_skip=True,
                    )
                    self.assertTrue(plan["security_required"])

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
        paths = ["AGENTS.md", "DEVELOPMENT.md", "docs/ci-performance.md", "docs/ci-emergency-relief.md", "docs/headless-testing.md"]
        for changes in [[path] for path in paths] + [paths + ["docs/designs/ci.md"]]:
            with self.subTest(changes=changes):
                plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
                self.assertFalse(plan["rust_required"])
                self.assertEqual(plan["selected_count"], 0)
                self.assertEqual(plan["matrix"], {"include": []})

    def test_headless_guide_and_agent_instructions_need_no_native_jobs(self) -> None:
        changes = ["AGENTS.md", "docs/headless-testing.md"]
        for name in changes:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("Developer test commands.\n")
        plan = ci_select.make_plan(
            self.metadata, self.root, changes, 2, allow_security_skip=True)
        self.assertFalse(plan["rust_required"])
        self.assertFalse(plan["security_required"])
        self.assertEqual(plan["matrix"], {"include": []})
        (self.root / changes[1]).unlink()
        deleted = ci_select.make_plan(
            self.metadata, self.root, changes, 2, allow_security_skip=True)
        self.assertTrue(deleted["security_required"])

    def test_developer_docs_do_not_mask_mixed_changes_or_full_override(self) -> None:
        for doc in ["AGENTS.md", "DEVELOPMENT.md", "docs/ci-performance.md", "docs/ci-emergency-relief.md", "docs/headless-testing.md"]:
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

    def install_builder_recipe(self) -> None:
        recipe = self.root / "os/Containerfile.builder"
        recipe.parent.mkdir(parents=True, exist_ok=True)
        recipe.write_text("FROM scratch\n")

    def test_builder_recipe_only_omits_compile_in_unchanged_image(self) -> None:
        self.install_builder_recipe()
        self.install_quality_helpers()
        for extras in ([], ["docs/ci-performance.md"], ["tools/ci/affected_packages.py"]):
            with self.subTest(extras=extras):
                plan = ci_select.make_plan(
                    self.metadata, self.root, ["os/Containerfile.builder", *extras], 2)
                self.assertFalse(plan["rust_required"])
                self.assertEqual(plan["selected"], [])
                self.assertEqual(plan["tested"], [])
                self.assertEqual(plan["matrix"], {"include": []})
                self.assertIn("candidate image qualification is separate", plan["reason"])

    def test_builder_recipe_mixed_inputs_and_deletion_retain_full_checks(self) -> None:
        self.install_builder_recipe()
        for extra in ("leaf/src/lib.rs", "Cargo.lock", ".cargo/config.toml",
                      "rust-toolchain.toml", ".forgejo/workflows/check.yml"):
            with self.subTest(extra=extra):
                plan = ci_select.make_plan(
                    self.metadata, self.root, ["os/Containerfile.builder", extra], 2)
                self.assertTrue(plan["rust_required"])
                self.assertTrue(plan["full"])
                self.assertEqual(plan["selected_count"], plan["workspace_count"])
        (self.root / "os/Containerfile.builder").unlink()
        deleted = ci_select.make_plan(self.metadata, self.root, ["os/Containerfile.builder"], 2)
        self.assertTrue(deleted["full"])
        self.assertTrue(deleted["rust_required"])

    def test_builder_recipe_explicit_acceptance_checks_both_architectures(self) -> None:
        self.install_builder_recipe()
        plan = ci_select.make_plan(
            self.metadata, self.root, ["os/Containerfile.builder"], 2, force_full=True)
        self.assertTrue(plan["full"])
        self.assertEqual(plan["selected_count"], plan["workspace_count"])
        self.assertEqual(plan["tested"], plan["selected"])
        self.assertEqual({row["arch"] for row in plan["matrix"]["include"]},
                         set(ci_select.NATIVE_RUNNERS))

    def test_build_input_forces_full_workspace(self) -> None:
        plan = ci_select.make_plan(self.metadata, self.root, ["Cargo.lock"], 2)
        self.assertTrue(plan["full"])
        self.assertEqual(plan["selected_count"], plan["workspace_count"])
        self.assertEqual(plan["shard_count"], 1)  # small workspace stays cheap

    def test_global_inputs_balance_large_workspaces_without_losing_coverage(self):
        # Enough genuine package targets/dependencies to exceed the budget,
        # with uneven costs so this exercises the production balancer.
        for index in range(100):
            name = f"app-{index:03}"
            self.metadata["packages"].append({
                "name": name,
                "manifest_path": str(self.root / "apps" / name / "Cargo.toml"),
                "targets": [{"name": name}] * (1 + index % 5),
                "dependencies": [{"name": f"external-{i}"} for i in range(index % 30)],
            })
        for path in ("Cargo.lock", "Cargo.toml", "rust-toolchain.toml", "crates/unmapped/src/lib.rs"):
            with self.subTest(path=path):
                plan = ci_select.make_plan(self.metadata, self.root, [path], 99)
                rows = plan["matrix"]["include"]
                self.assertEqual(plan["shard_count"], 2)
                self.assertEqual(len(rows), 2)
                self.assertEqual({row["arch"] for row in rows}, {"x86_64"})
                self.assertEqual({row["runner"] for row in rows}, {ci_select.NATIVE_RUNNERS["x86_64"]})
                for field in ("packages", "test_packages"):
                    names = [name for row in rows for name in row[field].split()[1::2]]
                    self.assertEqual(sorted(names), plan["selected"])
                    self.assertEqual(len(names), len(set(names)))
                self.assertEqual(sum(row["estimated_cost"] for row in rows), plan["estimated_cost"])
                self.assertLess(max(row["estimated_cost"] for row in rows), plan["estimated_cost"] * .6)
                reversed_metadata = {"packages": list(reversed(self.metadata["packages"]))}
                self.assertEqual(plan, ci_select.make_plan(reversed_metadata, self.root, [path], 99))
        single = ci_select.make_plan(self.metadata, self.root, ["Cargo.lock"], 1)
        self.assertEqual(single["shard_count"], 1)
        # A normal narrow edit does not fan out just because the workspace grew.
        narrow = ci_select.make_plan(self.metadata, self.root, ["leaf/src/lib.rs"], 99)
        self.assertEqual(narrow["shard_count"], 1)

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

    def install_control_inputs(self) -> None:
        for source, observers in ci_select.QUALITY_ONLY_CONTROL_INPUTS.items():
            for path in (source, *observers):
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
                    self.assertEqual(plan["selected"], [])
                    self.assertEqual(plan["tested"], [])
                    self.assertEqual(plan["selected_count"], 0)
                    self.assertEqual(plan["estimated_cost"], 0)
                    self.assertEqual(plan["shard_count"], 0)
                    self.assertEqual(plan["matrix"], {"include": []})
                    self.assertIn("mandatory repository-policy tests", plan["reason"])

    def test_ci_helper_exception_preserves_unknown_mixed_and_full_checks(self) -> None:
        self.install_quality_helpers()
        for source in ci_select.QUALITY_ONLY_HELPERS:
            for extra in ("tools/unknown.py", ".forgejo/workflows/check.yml",
                          "os/tests/security-boundaries.sh",
                          "leaf/src/lib.rs", "Cargo.lock", "tools/../tools/ci/affected_packages.py"):
                with self.subTest(source=source, extra=extra):
                    plan = ci_select.make_plan(self.metadata, self.root, [source, extra], 2)
                    self.assertTrue(plan["rust_required"])
                    if extra == "leaf/src/lib.rs":
                        self.assertIn("leaf", plan["selected"])
                    if extra == "Cargo.lock":
                        self.assertTrue(plan["full"])
                        self.assertEqual(plan["selected_count"], plan["workspace_count"])
            with self.subTest(source=source, force_full=True):
                full = ci_select.make_plan(self.metadata, self.root, [source], 2, force_full=True)
                self.assertTrue(full["full"])
                self.assertEqual(full["selected_count"], full["workspace_count"])
                self.assertEqual({row["arch"] for row in full["matrix"]["include"]},
                                 set(ci_select.NATIVE_RUNNERS))
        self.assertTrue(ci_select.make_plan(self.metadata, self.root, [], 2)["rust_required"])

    def test_missing_helper_or_test_retains_native_checks(self) -> None:
        for source, test in ci_select.QUALITY_ONLY_HELPERS.items():
            for missing in (source, test):
                self.install_quality_helpers()
                (self.root / missing).unlink()
                with self.subTest(source=source, missing=missing):
                    plan = ci_select.make_plan(self.metadata, self.root, [source, test], 2)
                    self.assertTrue(plan["rust_required"])

    def test_ci_control_plane_change_does_not_compile_the_workspace(self) -> None:
        self.install_quality_helpers()
        self.install_control_inputs()
        recipe = self.root / "os/Containerfile.builder"
        recipe.parent.mkdir(parents=True, exist_ok=True)
        recipe.write_text("FROM scratch\n")
        # Exact file set from PR #86, whose first two checks selected all 140
        # Rust packages even though every change was CI orchestration or its test.
        changes = [
            ".forgejo/workflows/check.yml",
            ".forgejo/workflows/legacy-app-e2e.yml",
            ".forgejo/workflows/tilo-relay.yaml",
            "os/Containerfile.builder",
            "os/tests/test_builder_candidate_workflow.py",
            "tools/ci/affected_packages.py",
            "tools/policy/run.sh",
            "tools/tests/test_ci_check_aggregate.py",
            "tools/tests/test_ci_queue_relief.py",
            "tools/tests/test_ci_receipt.sh",
            "tools/tests/test_ci_select.py",
            "tools/tests/test_workflow_package_paths.py",
        ]
        plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
        self.assertFalse(plan["rust_required"])
        self.assertFalse(plan["full"])
        self.assertEqual(plan["selected"], [])
        self.assertEqual(plan["matrix"], {"include": []})

    def test_mold_builder_policy_does_not_compile_unrelated_rust(self) -> None:
        self.install_quality_helpers()
        self.install_control_inputs()
        for path in (
            ".forgejo/workflows/build-builder-image.yml",
            "os/tests/test_builder_candidate_workflow.py",
            "tools/ci/native-mold-linker.sh",
            "tools/ci/verify-native-mold.sh",
            "tools/tests/test_ci_native_mold.py",
        ):
            target = self.root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text("checked by mandatory policy\n")
        recipe = self.root / "os/Containerfile.builder"
        recipe.parent.mkdir(parents=True, exist_ok=True)
        recipe.write_text("FROM scratch\n")
        changes = [
            ".forgejo/workflows/build-builder-image.yml",
            "os/Containerfile.builder",
            "os/tests/test_builder_candidate_workflow.py",
            "tools/ci/native-mold-linker.sh",
            "tools/ci/verify-native-mold.sh",
            "tools/tests/test_ci_native_mold.py",
        ]
        plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
        self.assertFalse(plan["rust_required"])
        self.assertEqual(plan["selected"], [])
        self.assertEqual(plan["matrix"], {"include": []})

    def test_builder_cache_policy_does_not_compile_unrelated_rust(self) -> None:
        self.install_quality_helpers()
        self.install_control_inputs()
        changes = [
            ".forgejo/workflows/build-builder-image.yml",
            "tools/tests/test_cargo_download_cache_action.py",
        ]
        plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
        self.assertFalse(plan["rust_required"])

    def test_builder_reuse_policy_does_not_compile_unrelated_rust(self) -> None:
        self.install_quality_helpers()
        self.install_control_inputs()
        changes = [
            ".forgejo/workflows/build-builder-image.yml",
            "os/tests/test_builder_candidate_workflow.py",
            "os/tests/release_pipeline_invariants.sh",
            "os/tests/workflow_dependency_invariants.sh",
            "tools/ci/builder_workflow_plan.py",
            "tools/ci/resolve_promoted_builder.py",
            "tools/tests/test_builder_workflow_plan.py",
            "tools/tests/test_cargo_download_cache_action.py",
            "tools/tests/test_cosign_install_action.py",
            "tools/tests/test_resolve_promoted_builder.py",
        ]
        plan = ci_select.make_plan(self.metadata, self.root, changes, 2)
        self.assertFalse(plan["rust_required"])
        self.assertEqual(plan["selected"], [])
        self.assertEqual(plan["matrix"], {"include": []})

    def test_builder_only_asset_rows_do_not_compile_unrelated_rust(self) -> None:
        subprocess.run(["git", "init", "-q", "-b", "base"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.email", "ci@example.invalid"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.name", "CI"], cwd=self.root, check=True)
        manifest = self.root / "os/external-assets/github-assets.tsv"
        manifest.parent.mkdir(parents=True)
        manifest.write_text(
            "a" * 64 + "\tpdfium.tgz\thttps://example.invalid/pdfium.tgz\n"
        )
        subprocess.run(["git", "add", str(manifest.relative_to(self.root))], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "base manifest"], cwd=self.root, check=True)
        base = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()

        manifest.write_text(
            manifest.read_text()
            + "# Builder-only linker archives.\n"
            + "b" * 64
            + "\tmold-2.40.4-x86_64-linux.tar.gz"
            + "\thttps://github.com/rui314/mold/releases/download/v2.40.4/"
            + "mold-2.40.4-x86_64-linux.tar.gz\n"
            + "c" * 64
            + "\tmold-2.40.4-aarch64-linux.tar.gz"
            + "\thttps://github.com/rui314/mold/releases/download/v2.40.4/"
            + "mold-2.40.4-aarch64-linux.tar.gz\n"
        )
        subprocess.run(["git", "commit", "-qam", "add mold assets"], cwd=self.root, check=True)
        head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()

        self.assertTrue(ci_select.builder_only_asset_manifest_change(self.root, base, head))
        self.install_quality_helpers()
        self.install_control_inputs()
        recipe = self.root / "os/Containerfile.builder"
        recipe.parent.mkdir(parents=True, exist_ok=True)
        recipe.write_text("FROM scratch\n")
        changes = [
            ".forgejo/workflows/build-builder-image.yml",
            "os/Containerfile.builder",
            "os/external-assets/github-assets.tsv",
            "os/tests/test_builder_candidate_workflow.py",
            "tools/ci/verify-native-mold.sh",
            "tools/tests/test_ci_native_mold.py",
        ]
        plan = ci_select.make_plan(
            self.metadata, self.root, changes, 2, builder_only_external_assets=True
        )
        self.assertFalse(plan["rust_required"])
        self.assertFalse(plan["full"])
        self.assertEqual(plan["matrix"], {"include": []})

    def test_shared_or_mixed_asset_changes_force_full_workspace(self) -> None:
        subprocess.run(["git", "init", "-q", "-b", "base"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.email", "ci@example.invalid"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.name", "CI"], cwd=self.root, check=True)
        manifest = self.root / "os/external-assets/github-assets.tsv"
        manifest.parent.mkdir(parents=True)
        manifest.write_text(
            "a" * 64 + "\tpdfium-old.tgz\thttps://example.invalid/pdfium-old.tgz\n"
        )
        subprocess.run(["git", "add", str(manifest.relative_to(self.root))], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "base manifest"], cwd=self.root, check=True)
        base = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()
        manifest.write_text(
            manifest.read_text()
            + "b" * 64
            + "\tmold-2.40.4-x86_64-linux.tar.gz"
            + "\thttps://github.com/rui314/mold/releases/download/v2.40.4/"
            + "mold-2.40.4-x86_64-linux.tar.gz\n"
            + "c" * 64 + "\tpdfium-new.tgz\thttps://example.invalid/pdfium-new.tgz\n"
        )
        subprocess.run(["git", "commit", "-qam", "mixed assets"], cwd=self.root, check=True)
        head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()

        self.assertFalse(ci_select.builder_only_asset_manifest_change(self.root, base, head))
        plan = ci_select.make_plan(
            self.metadata, self.root, ["os/external-assets/github-assets.tsv"], 2
        )
        self.assertTrue(plan["rust_required"])
        self.assertTrue(plan["full"])
        self.assertEqual(plan["selected_count"], plan["workspace_count"])

        manifest.write_text(
            "a" * 64 + "\tpdfium-old.tgz\thttps://example.invalid/pdfium-old.tgz\n"
            + "d" * 64
            + "\tmold-2.40.4-aarch64-linux.tar.gz"
            + "\thttps://example.invalid/mold-2.40.4-aarch64-linux.tar.gz\n"
        )
        subprocess.run(["git", "commit", "-qam", "untrusted mold source"], cwd=self.root, check=True)
        untrusted_head = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=self.root, text=True
        ).strip()
        self.assertFalse(
            ci_select.builder_only_asset_manifest_change(self.root, base, untrusted_head)
        )

    def test_ci_control_plane_mapping_stays_exact_and_conservative(self) -> None:
        self.install_control_inputs()
        source = ".forgejo/workflows/check.yml"
        for extra in (".forgejo/workflows/unknown.yml", "leaf/src/lib.rs",
                      "Cargo.lock", ".cargo/config.toml"):
            with self.subTest(extra=extra):
                plan = ci_select.make_plan(self.metadata, self.root, [source, extra], 2)
                self.assertTrue(plan["rust_required"])
        for mapped_source, observers in ci_select.QUALITY_ONLY_CONTROL_INPUTS.items():
            for missing in (mapped_source, *observers):
                with self.subTest(source=mapped_source, missing=missing):
                    self.install_control_inputs()
                    (self.root / missing).unlink()
                    plan = ci_select.make_plan(
                        self.metadata, self.root, [mapped_source], 2)
                    self.assertTrue(plan["rust_required"])

    def test_builder_recipe_deletion_and_mixed_inputs_stay_conservative(self) -> None:
        recipe = self.root / "os/Containerfile.builder"
        recipe.parent.mkdir(parents=True, exist_ok=True)
        recipe.write_text("FROM scratch\n")
        for extra in ("leaf/src/lib.rs", "Cargo.lock", ".cargo/config.toml",
                      ".forgejo/workflows/unknown.yml"):
            with self.subTest(extra=extra):
                plan = ci_select.make_plan(
                    self.metadata, self.root, ["os/Containerfile.builder", extra], 2)
                self.assertTrue(plan["rust_required"])
                self.assertTrue(plan["full"])
        recipe.unlink()
        deleted = ci_select.make_plan(
            self.metadata, self.root, ["os/Containerfile.builder"], 2)
        self.assertTrue(deleted["rust_required"])
        self.assertTrue(deleted["full"])

    def test_builder_observer_runs_in_mandatory_policy_suite(self) -> None:
        policy = (REPO_ROOT / "tools/policy/run.sh").read_text()
        self.assertIn(
            "run 'Builder candidate workflow tests' python3 "
            "os/tests/test_builder_candidate_workflow.py",
            policy,
        )

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
        self.assertEqual(json.loads(values["clippy_matrix"]), plan["matrix"])

    def test_clippy_matrix_lints_each_package_set_once_on_x86(self) -> None:
        self.install_builder_recipe()
        plan = ci_select.make_plan(
            self.metadata, self.root, ["os/Containerfile.builder"], 2, force_full=True)
        rows = plan["matrix"]["include"]
        self.assertEqual({row["arch"] for row in rows}, set(ci_select.NATIVE_RUNNERS))
        clippy = plan["clippy_matrix"]["include"]
        self.assertEqual(clippy, [row for row in rows if row["arch"] == "x86_64"])
        self.assertEqual(sorted(row["packages"] for row in clippy),
                         sorted(row["packages"] for row in rows if row["arch"] == "aarch64"))
        for row in rows:
            self.assertEqual(row["check_runner"], ci_select.NATIVE_RUNNERS[row["arch"]])

    def test_changed_files_fails_when_merge_base_is_unavailable(self) -> None:
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

        with self.assertRaisesRegex(RuntimeError, "merge base"):
            ci_select.changed_files(self.root, base, head)

    def test_changed_files_excludes_changes_only_on_advanced_base(self) -> None:
        subprocess.run(["git", "init", "-q", "-b", "base"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.email", "ci@example.invalid"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.name", "CI"], cwd=self.root, check=True)
        (self.root / "shared.txt").write_text("common\n")
        subprocess.run(["git", "add", "shared.txt"], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "common"], cwd=self.root, check=True)

        subprocess.run(["git", "switch", "-qc", "pull-request"], cwd=self.root, check=True)
        (self.root / "head-only.py").write_text("print('head')\n")
        subprocess.run(["git", "add", "head-only.py"], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "pull request"], cwd=self.root, check=True)
        head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()

        subprocess.run(["git", "switch", "-q", "base"], cwd=self.root, check=True)
        (self.root / "base-only.rs").write_text("fn landed_on_main() {}\n")
        subprocess.run(["git", "add", "base-only.rs"], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "advance base"], cwd=self.root, check=True)
        advanced_base = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=self.root, text=True
        ).strip()

        self.assertEqual(ci_select.changed_files(self.root, advanced_base, head), ["head-only.py"])

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


REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
LOCAL_PACKAGES = ("base", "middle", "leaf", "qdoc", "pkg", "collab-types")


def lockfile(external: dict[tuple[str, str], list[str]], local: dict[str, list[str]],
             header: str = "version = 4\n") -> bytes:
    """Render a Cargo.lock with registry packages and source-less workspace nodes."""
    blocks = [header]
    for (name, version), deps in sorted(external.items()):
        blocks.append(
            f'[[package]]\nname = "{name}"\nversion = "{version}"\nsource = "{REGISTRY}"\n'
            f'checksum = "{name}-{version}"\ndependencies = {deps!r}\n'.replace("'", '"'))
    for name, deps in sorted(local.items()):
        blocks.append(
            f'[[package]]\nname = "{name}"\nversion = "0.1.0"\ndependencies = {deps!r}\n'
            .replace("'", '"'))
    return "\n".join(blocks).encode()


ROOT_MANIFEST = """\
[workspace]
resolver = "3"
members = ["base", "middle", "leaf", "qdoc", "pkg", "collab-types"]

[workspace.package]
edition = "2024"

[workspace.lints.clippy]
pedantic = "deny"

[workspace.dependencies]
itoa = "1.0"
serde = { version = "1", features = ["derive"] }
json = { package = "serde_json", version = "1" }

[profile.release]
lto = "thin"

[patch.crates-io]
vendored-sys = { path = "vendor/vendored-sys" }
"""


class ManifestAttributionTests(unittest.TestCase):
    """Root Cargo.toml and Cargo.lock changes select only what they reach."""

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / "Cargo.toml").write_text(ROOT_MANIFEST)
        self.metadata = {"packages": []}
        for name, deps in [("base", [("serde", None)]), ("middle", [("base", "path"), ("itoa", None)]),
                           ("leaf", [("middle", "path"), ("serde_json", "json")]),
                           ("qdoc", []), ("pkg", []), ("collab-types", [])]:
            self.add_package(name, deps)
        self.external = {
            ("serde", "1.0.0"): [],
            ("itoa", "1.0.0"): [],
            ("serde_json", "1.0.0"): ["itoa", "serde"],
            ("vendored-sys", "0.9.6"): ["cc"],
            ("cc", "1.2.0"): [],
        }
        self.local = {
            "base": ["serde"], "middle": ["base", "itoa"], "leaf": ["middle", "serde_json"],
            "qdoc": [], "pkg": [], "collab-types": [],
        }

    def tearDown(self) -> None:
        self.temp.cleanup()

    def add_package(self, name: str, deps: list[tuple[str, str | None]]) -> None:
        dependencies = []
        for dep, kind in deps:
            entry = {"name": dep, "rename": None}
            if kind == "path":
                entry["path"] = str(self.root / dep)
            elif kind:
                entry["rename"] = kind
            dependencies.append(entry)
        self.metadata["packages"].append({
            "name": name,
            "manifest_path": str(self.root / name / "Cargo.toml"),
            "targets": [{"name": name}],
            "dependencies": dependencies,
        })

    def lock(self, external=None, local=None, header="version = 4\n") -> bytes:
        return lockfile(self.external if external is None else external,
                        self.local if local is None else local, header)

    def plan(self, changes: list[str], resolved: dict) -> dict:
        return ci_select.make_plan(self.metadata, self.root, changes, 2, resolved_inputs=resolved)

    def test_adding_a_workspace_crate_selects_only_the_new_crate(self) -> None:
        self.add_package("durable", [("serde", None), ("thiserror", None)])
        before = self.lock()
        after = self.lock(external={**self.external, ("thiserror", "2.0.0"): []},
                          local={**self.local, "durable": ["serde", "thiserror"]})
        manifest = ROOT_MANIFEST.replace('"collab-types"]', '"collab-types", "durable"]')
        resolved = {
            "Cargo.lock": ci_select.resolve_lockfile(before, after, self.metadata),
            "Cargo.toml": ci_select.resolve_root_manifest(
                ROOT_MANIFEST.encode(), manifest.encode(), self.metadata, self.root),
        }
        self.assertEqual(resolved["Cargo.lock"].seeds, {"durable"})
        self.assertEqual(resolved["Cargo.toml"].seeds, {"durable"})
        plan = self.plan(["Cargo.lock", "Cargo.toml", "durable/Cargo.toml", "durable/src/lib.rs"], resolved)
        self.assertFalse(plan["full"])
        self.assertEqual(set(plan["selected"]), {"durable"} | ci_select.ALWAYS_PACKAGES)
        self.assertEqual(set(plan["tested"]), {"durable"} | ci_select.ALWAYS_PACKAGES)

    def test_leaf_dependency_bump_selects_its_consumers_and_their_dependants(self) -> None:
        after = self.lock(external={**{k: v for k, v in self.external.items() if k[0] != "itoa"},
                                    ("itoa", "1.0.1"): []})
        resolved = {"Cargo.lock": ci_select.resolve_lockfile(self.lock(), after, self.metadata)}
        # itoa is used directly by middle and through serde_json by leaf.
        self.assertEqual(resolved["Cargo.lock"].seeds, {"middle", "leaf"})
        plan = self.plan(["Cargo.lock"], resolved)
        self.assertFalse(plan["full"])
        self.assertEqual(set(plan["selected"]), {"middle", "leaf"} | ci_select.ALWAYS_PACKAGES)
        self.assertEqual(set(plan["tested"]), {"middle", "leaf"} | ci_select.ALWAYS_PACKAGES)
        self.assertNotIn("base", plan["selected"])

    def test_transitive_dependency_change_reaches_workspace_consumers(self) -> None:
        external = dict(self.external)
        external[("serde", "1.0.0")] = ["serde_derive"]
        external[("serde_derive", "1.0.0")] = []
        before = self.lock(external=external)
        external.pop(("serde_derive", "1.0.0"))
        external[("serde_derive", "1.0.1")] = []
        resolved = ci_select.resolve_lockfile(before, self.lock(external=external), self.metadata)
        self.assertEqual(resolved.seeds, {"base", "leaf"})
        self.assertEqual(set(self.plan(["Cargo.lock"], {"Cargo.lock": resolved})["selected"]),
                         {"base", "middle", "leaf"} | ci_select.ALWAYS_PACKAGES)

    def test_second_version_of_a_crate_does_not_touch_existing_consumers(self) -> None:
        self.add_package("legacy", [("itoa", None)])
        local = {**self.local, "middle": ["base", "itoa 1.0.0"], "leaf": ["middle", "serde_json"]}
        external = {**self.external, ("itoa", "0.4.0"): [],
                    ("serde_json", "1.0.0"): ["itoa 1.0.0", "serde"]}
        after = self.lock(external=external, local={**local, "legacy": ["itoa 0.4.0"]})
        resolved = ci_select.resolve_lockfile(
            self.lock(local={**self.local, "legacy": []}), after, self.metadata)
        self.assertEqual(resolved.seeds, {"legacy"})

    def test_removing_a_dependency_selects_its_former_consumers(self) -> None:
        external = {k: v for k, v in self.external.items() if k[0] != "itoa"}
        external[("serde_json", "1.0.0")] = ["serde"]
        after = self.lock(external=external, local={**self.local, "middle": ["base"]})
        resolved = ci_select.resolve_lockfile(self.lock(), after, self.metadata)
        self.assertEqual(resolved.seeds, {"middle", "leaf"})

    def test_removing_a_workspace_member_selects_its_dependants(self) -> None:
        manifest = ROOT_MANIFEST.replace('"base", "middle"', '"middle"')
        self.metadata["packages"] = [p for p in self.metadata["packages"] if p["name"] != "base"]
        resolved = ci_select.resolve_root_manifest(
            ROOT_MANIFEST.encode(), manifest.encode(), self.metadata, self.root)
        self.assertEqual(resolved.seeds, {"middle"})
        plan = self.plan(["Cargo.toml"], {"Cargo.toml": resolved})
        self.assertFalse(plan["full"])
        self.assertEqual(set(plan["selected"]), {"middle", "leaf"} | ci_select.ALWAYS_PACKAGES)

    def test_workspace_dependency_change_selects_members_that_use_it(self) -> None:
        for old, new, expected in [
            ('itoa = "1.0"', 'itoa = "1.0.9"', {"middle"}),
            ('features = ["derive"]', 'features = ["derive", "rc"]', {"base"}),
            # A renamed entry is matched through both its key and package.
            ('json = { package = "serde_json", version = "1" }',
             'json = { package = "serde_json", version = "1.0.100" }', {"leaf"}),
            ('itoa = "1.0"\n', 'itoa = "1.0"\nryu = "1"\n', set()),
        ]:
            with self.subTest(new=new):
                resolved = ci_select.resolve_root_manifest(
                    ROOT_MANIFEST.encode(), ROOT_MANIFEST.replace(old, new).encode(),
                    self.metadata, self.root)
                self.assertIsNotNone(resolved)
                self.assertEqual(resolved.seeds, expected)

    def test_unattributable_root_manifest_changes_keep_full_selection(self) -> None:
        for old, new in [
            ('pedantic = "deny"', 'pedantic = "warn"'),
            ('lto = "thin"', 'lto = "fat"'),
            ('edition = "2024"', 'edition = "2024"\nrust-version = "1.90"'),
            ('vendored-sys = { path = "vendor/vendored-sys" }', ''),
            ('resolver = "3"', 'resolver = "2"'),
            ('members = [', 'exclude = ["fuzz"]\nmembers = ['),
            ('"collab-types"]', '"collab-types", "crates/*"]'),
            ('"collab-types"]', '"collab-types", "unknown"]'),
            ('[workspace]', '[workspace]\n[workspace.metadata.tool]\nx = 1\n[workspace.x]'),
            ('[workspace]', '[package]\nname = "root"\n[workspace]'),
            ('[workspace]', '[workspace'),
        ]:
            with self.subTest(new=new):
                resolved = ci_select.resolve_root_manifest(
                    ROOT_MANIFEST.encode(), ROOT_MANIFEST.replace(old, new).encode(),
                    self.metadata, self.root)
                self.assertIsNone(resolved)
                plan = self.plan(["Cargo.toml"], {})
                self.assertTrue(plan["full"])
                self.assertEqual(plan["selected_count"], plan["workspace_count"])
        self.assertIsNone(ci_select.resolve_root_manifest(
            None, ROOT_MANIFEST.encode(), self.metadata, self.root))

    def test_malformed_or_unprovable_lockfiles_keep_full_selection(self) -> None:
        good = self.lock()
        ambiguous = self.lock(external={**self.external, ("itoa", "0.4.0"): []})
        for before, after in [
            (good, b"[[package]\nname = "),
            (b"\xff\xfe", good),
            (None, good),
            (good, None),
            (good, self.lock(header="version = 3\n")),
            # Two itoa records make the unqualified "itoa" references ambiguous.
            (good, ambiguous),
            (good, self.lock(local={**self.local, "middle": ["base", "missing"]})),
            (good, good.replace(b'name = "cc"', b'name_typo = "cc"')),
            # Attribution requires every workspace member to be a lockfile node.
            (good, self.lock(local={k: v for k, v in self.local.items() if k != "qdoc"})),
        ]:
            with self.subTest(after=after and after[:40]):
                self.assertIsNone(ci_select.resolve_lockfile(before, after, self.metadata))
        plan = self.plan(["Cargo.lock"], {})
        self.assertTrue(plan["full"])
        self.assertIn("repository-wide", plan["reason"])

    def test_lockfile_change_reaching_no_workspace_crate_selects_only_contract_suites(self) -> None:
        after = self.lock(external={**{k: v for k, v in self.external.items() if k[0] != "cc"},
                                    ("cc", "1.2.1"): []})
        resolved = ci_select.resolve_lockfile(self.lock(), after, self.metadata)
        self.assertEqual(resolved.seeds, set())
        plan = self.plan(["Cargo.lock"], {"Cargo.lock": resolved})
        self.assertFalse(plan["full"])

    def test_attribution_never_hides_other_global_inputs_or_explicit_full(self) -> None:
        after = self.lock(external={**self.external, ("unused", "1.0.0"): []})
        resolved = {"Cargo.lock": ci_select.resolve_lockfile(self.lock(), after, self.metadata)}
        for changes in (["Cargo.lock", "rust-toolchain.toml"], ["Cargo.lock", ".cargo/config.toml"],
                        ["Cargo.lock", "Cargo.toml"]):
            with self.subTest(changes=changes):
                plan = self.plan(changes, resolved)
                self.assertTrue(plan["full"])
        full = ci_select.make_plan(self.metadata, self.root, ["Cargo.lock"], 2,
                                   force_full=True, resolved_inputs=resolved)
        self.assertTrue(full["full"])
        self.assertEqual(full["qualification"], "full-native")
        # A resolution for a path that did not change is ignored.
        self.assertEqual(self.plan(["leaf/src/lib.rs"], resolved)["tested"],
                         sorted({"leaf"} | ci_select.ALWAYS_PACKAGES))

    def test_git_revisions_feed_manifest_attribution(self) -> None:
        def git(*args: str) -> str:
            return subprocess.check_output(["git", *args], cwd=self.root, text=True).strip()
        git("init", "-q", "-b", "main")
        git("config", "user.email", "ci@example.invalid")
        git("config", "user.name", "CI")
        (self.root / "Cargo.lock").write_bytes(self.lock())
        git("add", "Cargo.toml", "Cargo.lock")
        git("commit", "-qm", "base")
        base = git("rev-parse", "HEAD")
        after = self.lock(external={**{k: v for k, v in self.external.items() if k[0] != "itoa"},
                                    ("itoa", "1.0.1"): []})
        (self.root / "Cargo.lock").write_bytes(after)
        (self.root / "Cargo.toml").write_text(ROOT_MANIFEST.replace('itoa = "1.0"', 'itoa = "1.0.1"'))
        git("commit", "-qam", "bump itoa")
        changes = ci_select.changed_files(self.root, base, "HEAD")
        resolved = ci_select.resolve_manifest_inputs(self.root, base, "HEAD", self.metadata, changes)
        self.assertEqual(set(resolved), {"Cargo.lock", "Cargo.toml"})
        plan = self.plan(changes, resolved)
        self.assertFalse(plan["full"])
        self.assertEqual(set(plan["selected"]), {"middle", "leaf"} | ci_select.ALWAYS_PACKAGES)
        self.assertIn("Cargo.lock", plan["reason"])
        # An unreadable head lockfile is unattributable, so it stays global.
        (self.root / "Cargo.lock").write_bytes(b"not toml [")
        git("commit", "-qam", "corrupt")
        changes = ci_select.changed_files(self.root, base, "HEAD")
        resolved = ci_select.resolve_manifest_inputs(self.root, base, "HEAD", self.metadata, changes)
        self.assertNotIn("Cargo.lock", resolved)
        self.assertTrue(self.plan(changes, resolved)["full"])


if __name__ == "__main__":
    unittest.main()
