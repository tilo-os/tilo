from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch, Mock
import json
import subprocess

SPEC = importlib.util.spec_from_file_location("compiler_inputs", Path(__file__).parents[1] / "ci/compiler_inputs.py")
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)


class CompilerInputsTests(unittest.TestCase):
    def test_make_escapes_continuations_and_phony_rules(self):
        text = "target/a.d: src/lib.rs fixture\\ space.txt \\\n src/hash\\#.txt cash$$.txt\nsrc/lib.rs:\n# env-dep:FOO=private\n"
        self.assertEqual(module.dep_inputs(text), ["cash$.txt", "fixture space.txt", "src/hash#.txt", "src/lib.rs"])

    def test_unsupported_rules_fail(self):
        for text in ("nonsense", "target/a.d: C:/source.rs"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                module.dep_inputs(text)

    def test_paths_cannot_escape_workspace(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            self.assertEqual(module.input_record(root / "../private", root), {"scope": "external", "status": "unknown"})
            self.assertEqual(module.input_record(root / "missing", root)["status"], "missing-or-directory")
            (root / "input").write_text("one")
            first = module.input_record(root / "input", root)
            (root / "input").write_text("two")
            self.assertNotEqual(first["sha256"], module.input_record(root / "input", root)["sha256"])

    def test_build_script_declared_files_and_env_names(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "schema").write_text("schema")
            row = module.build_inputs("cargo:rerun-if-changed=schema\ncargo::rerun-if-env-changed=TOKEN\n", root, root)
            self.assertEqual(row["inputs"][0]["path"], "schema")
            self.assertEqual(row["environment_names"], ["TOKEN"])
            self.assertTrue(row["unknown"])
            self.assertTrue(module.build_inputs("cargo:rustc-cfg=foo", root, root)["unknown"])

    def test_missing_build_script_output_is_explicit(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            metadata = {"workspace_members": ["id"], "packages": [{"id": "id", "name": "example", "manifest_path": str(root / "Cargo.toml")}]}
            receipt = module.collect([{"reason": "build-script-executed", "package_id": "id", "out_dir": str(root / "missing/out")}], metadata, root, {})
            self.assertEqual(receipt["build_scripts"][0]["unknown"], ["build-script output unavailable"])

    def test_workflow_retains_check_scope_and_uploads_before_clippy(self):
        workflow = (Path(__file__).parents[2] / ".forgejo/workflows/check.yml").read_text()
        check = workflow.index("python3 tools/ci/compiler_inputs.py")
        upload = workflow.index("- name: Save observed compiler inputs")
        clippy = workflow.index("- name: Clippy shard")
        self.assertLess(check, upload)
        self.assertLess(upload, clippy)
        self.assertIn("check --locked --all-targets ${{ matrix.packages }}", workflow[check:upload])
        self.assertIn("if: always()", workflow[upload:clippy])

    def test_target_binding_and_missing_observers(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "src").mkdir()
            (root / "src/lib.rs").write_text("pub fn f() {}")
            (root / "target").mkdir()
            (root / "target/example-123.d").write_text("target/libexample-123.rmeta: src/lib.rs\n")
            metadata = {"workspace_members": ["id"], "packages": [{"id": "id", "name": "example", "manifest_path": str(root / "Cargo.toml")}]}
            artifact = {"reason": "compiler-artifact", "package_id": "id", "target": {"name": "example", "kind": ["lib"], "src_path": str(root / "src/lib.rs")}, "profile": {"test": False}, "features": ["a"], "filenames": [str(root / "target/libexample-123.rmeta")]}
            messages = [artifact, {"reason": "build-finished", "success": True}]
            receipt = module.collect(messages, metadata, root, {"commit": "head", "dirty": False})
            self.assertEqual(receipt["binding"]["commit"], "head")
            self.assertEqual(receipt["targets"][0]["inputs"][0]["path"], "src/lib.rs")
            self.assertFalse(receipt["targets"][0]["unknown"])
            (root / "target/example-123.d").unlink()
            receipt = module.collect([artifact], metadata, root, {"dirty": True})
            self.assertTrue(receipt["targets"][0]["unknown"])
            self.assertIn("successful build-finished message missing", receipt["unknown"])
            self.assertIn("tracked worktree differs from bound commit", receipt["unknown"])

    def test_observer_preflight_failure_still_runs_check(self):
        with patch.object(module.sys, "argv", ["collector", "--output", "unused", "--", "check", "--locked"]), \
             patch.object(module, "command", side_effect=subprocess.CalledProcessError(1, "git")), \
             patch.object(module.subprocess, "call", return_value=101) as cargo:
            self.assertEqual(module.main(), 101)
            cargo.assert_called_once_with(["cargo", "check", "--locked"])

    def test_failed_cargo_status_and_receipt_are_preserved(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / "receipt.json"
            def command(*args):
                if args[:2] == ("cargo", "metadata"):
                    return json.dumps({"workspace_members": [], "packages": []})
                if args[:2] == ("git", "status"):
                    return ""
                return "binding"
            process = Mock(stdout=iter(['{"reason":"build-finished","success":false}\n']))
            process.wait.return_value = 101
            with patch.object(module.sys, "argv", ["collector", "--output", str(output), "--", "check", "--locked"]), \
                 patch.object(module, "command", side_effect=command), \
                 patch.object(module.subprocess, "Popen", return_value=process):
                self.assertEqual(module.main(), 101)
            receipt = json.loads(output.read_text())
            self.assertEqual(receipt["cargo_exit_code"], 101)
            self.assertIn("successful build-finished message missing", receipt["unknown"])
            self.assertEqual(receipt["purpose"], "observation-only")


if __name__ == "__main__":
    unittest.main()
