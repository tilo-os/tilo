"""Exact resource admission cannot remove coverage or admit unqualified work."""
import importlib.util
import json
import os
import re
import shutil
import tempfile
from pathlib import Path
import sys
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
from ci import qualified_native


class QualifiedNativeTests(unittest.TestCase):
    def test_native_admission_retains_general_lane_without_a_selection_wait(self):
        workflow = (ROOT / ".forgejo/workflows/check.yml").read_text()
        selection = workflow.split("  select:\n", 1)[1].split("  security:\n", 1)[0]
        self.assertNotIn("needs:", selection)
        self.assertNotIn("needs.builder", selection)
        expression = "matrix.runner"
        for name in ("test", "clippy"):
            job = re.split(r"\n  \S", workflow.split(f"  {name}:\n", 1)[1], maxsplit=1)[0]
            self.assertIn("needs: [builder, select, security]", job)
            self.assertIn("runs-on: ${{ " + expression + " }}", job)

    def test_real_input_binding_falls_back_on_builder_workflow_or_source_change(self):
        receipt = json.loads((ROOT / "tools/ci/qualified-native.json").read_text())
        workflow = (ROOT / ".forgejo/workflows/check.yml").read_bytes()
        # CI merges newer main inputs before running policy tests. Qualify a
        # bounded copy, then change an actual input: the production fingerprint
        # and admission must agree without assuming main remains frozen.
        parent = ROOT / ".task-tmp"
        parent.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="native-binding-", dir=parent) as temporary:
            root = Path(temporary)
            for name in qualified_native.INPUTS:
                destination = root / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / name, destination)
            for name in qualified_native.PACKAGES:
                shutil.copytree(ROOT / name, root / name)
            receipt["enabled"] = True
            receipt["input_sha256"] = qualified_native.fingerprint(root)
            (root / "tools/ci/qualified-native.json").write_text(json.dumps(receipt))
            with mock.patch.dict(os.environ, {"HEAD_SHA": "fixture"}), mock.patch.object(
                    qualified_native.subprocess, "check_output", return_value=workflow):
                self.assertEqual(qualified_native.placement(root, {"collab-types"}, "x86_64"),
                                 {"qualified_runner": "runner-quality", "qualified_builders": receipt["builders"]})
                receipt["enabled"] = False
                (root / "tools/ci/qualified-native.json").write_text(json.dumps(receipt))
                self.assertFalse(qualified_native.placement(root, {"collab-types"}, "x86_64"))
                del receipt["enabled"]
                (root / "tools/ci/qualified-native.json").write_text(json.dumps(receipt))
                self.assertFalse(qualified_native.placement(root, {"collab-types"}, "x86_64"))
                receipt["enabled"] = True
                (root / "tools/ci/qualified-native.json").write_text(json.dumps(receipt))
                for names, arch in (({"collab-types", "tilo-ui"}, "x86_64"), ({"collab-types"}, "aarch64")):
                    self.assertFalse(qualified_native.placement(root, names, arch))
                with mock.patch.object(qualified_native.subprocess, "check_output", return_value=b"older workflow"):
                    self.assertFalse(qualified_native.placement(root, {"collab-types"}, "x86_64"))
                helper = root / "tools/ci/clippy-check.sh"
                helper.write_text(helper.read_text() + "\n# changed runtime input\n")
                self.assertFalse(qualified_native.placement(root, {"collab-types"}, "x86_64"))

    def test_planner_preserves_tests_and_clippy_and_certifies_only_app_selections(self):
        spec = importlib.util.spec_from_file_location("ci_select_tests", ROOT / "tools/tests/test_ci_select.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        fixture = module.SelectorTests()
        fixture.setUp()
        try:
            planner = module.ci_select
            fixture.metadata["packages"].append({
                "name": "demo", "manifest_path": str(fixture.root / "apps/demo/Cargo.toml"),
                "targets": [{"name": "demo"}], "dependencies": []})
            fixture.metadata["packages"] = [p for p in fixture.metadata["packages"]
                                            if p["name"] in {"collab-types", "demo"}]
            with mock.patch.object(planner.qualified_native, "placement", side_effect=lambda root, names, arch:
                                   {"qualified_runner": "runner-quality", "qualified_builders": ["measured"]}
                                   if names == {"collab-types"} and arch == "x86_64" else {}):
                plan = planner.make_plan(fixture.metadata, fixture.root, ["collab-types/src/lib.rs"], 2)
                row = plan["matrix"]["include"][0]
                self.assertEqual(row["test_packages"], "-p collab-types")
                self.assertEqual(row["runner"], "runner")
                self.assertEqual(row["qualified_runner"], "runner-quality")
                self.assertFalse(row["certify"])
                with mock.patch.object(planner.qualified_native, "placement", return_value={}):
                    deferred = planner.make_plan(fixture.metadata, fixture.root, ["collab-types/src/lib.rs"], 2)
                for deferred_row in deferred["matrix"]["include"] + deferred["clippy_matrix"]["include"]:
                    self.assertEqual(deferred_row["runner"], "runner")
                    self.assertEqual(deferred_row["qualified_runner"], "")
                    self.assertEqual(deferred_row["qualified_builders"], "")
                self.assertEqual(plan["clippy_matrix"]["include"][0]["qualified_runner"], "runner-quality")
                app_plan = planner.make_plan(fixture.metadata, fixture.root, ["apps/demo/src/lib.rs"], 2)
                app_row = app_plan["matrix"]["include"][0]
                self.assertIn("-p demo", app_row["test_packages"])
                self.assertTrue(app_row["certify"])
                self.assertEqual(app_row["runner"], "runner")
        finally:
            fixture.doCleanups()


if __name__ == "__main__":
    unittest.main()
