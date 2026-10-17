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
    def test_make_and_build_script_inputs_are_parsed_strictly_within_the_workspace(self):
        with self.subTest(phase='make escapes continuations and phony rules'):
            text = "target/a.d: src/lib.rs fixture\\ space.txt \\\n src/hash\\#.txt cash$$.txt\nsrc/lib.rs:\n# env-dep:FOO=private\n"
            self.assertEqual(module.dep_inputs(text), ["cash$.txt", "fixture space.txt", "src/hash#.txt", "src/lib.rs"])
        with self.subTest(phase='unsupported rules fail'):
            for text in ("nonsense", "target/a.d: C:/source.rs"):
                with self.subTest(text=text), self.assertRaises(ValueError):
                    module.dep_inputs(text)
        with self.subTest(phase='paths cannot escape workspace'):
            with tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                self.assertEqual(module.input_record(root / "../private", root), {"scope": "external", "status": "unknown"})
                self.assertEqual(module.input_record(root / "missing", root)["status"], "missing-or-directory")
                (root / "input").write_text("one")
                first = module.input_record(root / "input", root)
                (root / "input").write_text("two")
                self.assertNotEqual(first["sha256"], module.input_record(root / "input", root)["sha256"])
        with self.subTest(phase='build script declared files and env names'):
            with tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                (root / "schema").write_text("schema")
                row = module.build_inputs("cargo:rerun-if-changed=schema\ncargo::rerun-if-env-changed=TOKEN\n", root, root)
                self.assertEqual(row["inputs"][0]["path"], "schema")
                self.assertEqual(row["environment_names"], ["TOKEN"])
                self.assertTrue(row["unknown"])
                self.assertTrue(module.build_inputs("cargo:rustc-cfg=foo", root, root)["unknown"])
        with self.subTest(phase='missing build script output is explicit'):
            with tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                metadata = {"workspace_members": ["id"], "packages": [{"id": "id", "name": "example", "manifest_path": str(root / "Cargo.toml")}]}
                receipt = module.collect([{"reason": "build-script-executed", "package_id": "id", "out_dir": str(root / "missing/out")}], metadata, root, {})
                self.assertEqual(receipt["build_scripts"][0]["unknown"], ["build-script output unavailable"])

    def test_optional_observer_cannot_replace_cargo_status_with_bad_structured_data(self):
        metadata = {"workspace_members": ["id"], "packages": [
            {"id": "id", "name": "example", "manifest_path": "Cargo.toml"}]}
        for status in (0, 101):
            for document, messages in (
                (metadata, [[], {"reason": "build-finished", "success": status == 0}]),
                (metadata, [{"reason": "compiler-artifact", "package_id": "id", "target": None}]),
                (metadata, [{"reason": "compiler-message", "message": None}]),
                (metadata, [{"reason": "compiler-message", "message": {"rendered": []}}]),
                ([], [{"reason": "build-finished", "success": status == 0}]),
            ):
                with self.subTest(status=status, document=document, messages=messages), tempfile.TemporaryDirectory() as folder:
                    def command(*args):
                        if args[:2] == ("cargo", "metadata"):
                            return json.dumps(document)
                        return "" if args[:2] == ("git", "status") else "head"
                    process = Mock(stdout=iter(json.dumps(message) + "\n" for message in messages))
                    process.wait.return_value = status
                    with patch.object(module.sys, "argv", ["collector", "--output", str(Path(folder) / "receipt.json"), "--", "check", "--locked"]), \
                         patch.object(module, "command", side_effect=command), \
                         patch.object(module.subprocess, "Popen", return_value=process):
                        self.assertEqual(module.main(), status)
                    process.wait.assert_called_once()

    def test_check_and_clippy_bind_inputs_and_preserve_every_failure(self):
        with self.subTest(phase='workflow retains check scope and uploads before clippy'):
            workflow = (Path(__file__).parents[2] / ".forgejo/workflows/check.yml").read_text()
            check = workflow.index("python3 tools/ci/compiler_inputs.py")
            upload = workflow.index("- name: Save observed compiler inputs")
            clippy = workflow.index("- name: Clippy shard")
            # Clippy records the same observation for the x86_64 check it replaces.
            self.assertIn('python3 "$root/tools/ci/compiler_inputs.py"',
                          (Path(__file__).parents[2] / "tools/ci/clippy-check.sh").read_text())
            self.assertLess(check, upload)
            self.assertLess(upload, clippy)
            self.assertIn("check --locked --all-targets ${{ matrix.packages }}", workflow[check:upload])
            self.assertIn("if: always()", workflow[upload:clippy])
        with self.subTest(phase='target binding and missing observers'):
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
        with self.subTest(phase='observer preflight failure still runs check'):
            with patch.object(module.sys, "argv", ["collector", "--output", "unused", "--", "check", "--locked"]), \
                 patch.object(module, "command", side_effect=subprocess.CalledProcessError(1, "git")), \
                 patch.object(module.subprocess, "call", return_value=101) as cargo:
                self.assertEqual(module.main(), 101)
                cargo.assert_called_once_with(["cargo", "check", "--locked"])
        with self.subTest(phase='existing clippy messages preserve failure and source binding'):
            for status, expected in ((0, "head"), (101, "head"), (0, "other")):
                with self.subTest(status=status, expected=expected), tempfile.TemporaryDirectory() as folder:
                    output = Path(folder) / "receipt.json"
                    messages = Path(folder) / "clippy.json"
                    messages.write_text(
                        json.dumps({"reason": "compiler-message", "message": {"level": "error", "rendered": "large diagnostic"}}) + "\n"
                        + json.dumps({"reason": "build-finished", "success": status == 0}) + "\n")
                    def command(*args):
                        if args[:2] == ("cargo", "metadata"):
                            return json.dumps({"workspace_members": [], "packages": []})
                        if args[:2] == ("git", "status"):
                            return ""
                        return "head"
                    with patch.object(module.sys, "argv", ["collector", "--output", str(output),
                            "--from-messages", str(messages), "--cargo-exit-code", str(status),
                            "--expected-commit", expected, "--", "clippy", "--locked", "--all-targets"]), \
                         patch.object(module, "command", side_effect=command), \
                         patch.object(module.subprocess, "Popen") as compile_again, \
                         patch.object(module, "collect", wraps=module.collect) as collected:
                        self.assertEqual(module.main(), 0)
                        compile_again.assert_not_called()
                        self.assertEqual(collected.call_args.args[0], [
                            {"reason": "build-finished", "success": status == 0}])
                    receipt = json.loads(output.read_text())
                    self.assertEqual(receipt["cargo_exit_code"], status)
                    self.assertEqual(receipt["binding"]["commit"], "head")
                    self.assertEqual(receipt["binding"]["expected_commit"], expected)
                    self.assertEqual("source revision differs from invocation start" in receipt["unknown"], expected != "head")
                    self.assertEqual("Cargo invocation failed; target coverage incomplete" in receipt["unknown"], status != 0)
                    self.assertEqual(receipt["purpose"], "observation-only")
                    self.assertEqual("successful build-finished message missing" in receipt["unknown"], status != 0)
        with self.subTest(phase='clippy collection precedes cleanup and upload follows clippy'):
            root = Path(__file__).parents[2]
            script = (root / "tools/ci/clippy-check.sh").read_text()
            body = script.split("run_clippy() {", 1)[1].split("print_diagnostics()", 1)[0]
            self.assertLess(body.index("source_commit=$(git rev-parse HEAD)"), body.index("cargo clippy"))
            self.assertLess(body.index("cargo clippy"), body.index('--from-messages "$messages"'))
            self.assertLess(body.index('--from-messages "$messages"'), body.index('return "$result"'))
            self.assertIn('receipt_name=clippy-head-inputs.json', body)
            self.assertIn('"$root/target/ci/$receipt_name"', body)
            workflow = (root / ".forgejo/workflows/check.yml").read_text()
            upload = workflow.index("- name: Save observed Clippy inputs")
            self.assertLess(workflow.index("- name: Clippy shard"), upload)
            self.assertIn("if: always()", workflow[upload:upload + 400])
            self.assertIn("path: target/ci/clippy-*-inputs.json", workflow[upload:upload + 500])
        with self.subTest(phase='failed cargo status and receipt are preserved'):
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
