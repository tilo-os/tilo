"""Exercise production artifact handoff and consumer routing without Rust/VMs."""

import hashlib
import importlib.util
import json
import os
import sys
import tempfile
import unittest
import subprocess
from pathlib import Path
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import layer_artifacts as artifacts  # noqa: E402
from acceptance.vm import VmError  # noqa: E402

spec = importlib.util.spec_from_file_location("acceptance_run", HERE / "run.py")
run = importlib.util.module_from_spec(spec)
spec.loader.exec_module(run)
SOURCE = "a" * 40
BUILDER = "registry.tilo.so/tilo/tilo-builder@sha256:" + "b" * 64


class HandoffTests(unittest.TestCase):
    def setUp(self):
        root = Path(os.environ.get("CARGO_TARGET_DIR") or HERE.parents[1] / "target") / "layer-artifact-tests"
        root.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=root)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.expected = artifacts.identities(SOURCE, BUILDER, 1234, 1)
        self.layers = []
        for name in artifacts.COHORT:
            inventory = self.root / f"{name}.inputs.json"
            inventory.write_text(json.dumps(dict(provenance=dict(component=name,
                                 source_commit=SOURCE, dirty_tree_hash=None), files={"Cargo.lock": "c" * 64})))
            packed = dict(component=name, target="x86_64-unknown-linux-gnu", toolchain="rustc fixture",
                          source_digest=artifacts.sha(inventory), layer_hash="d" * 64,
                          reused=False, binary=False)
            stream = self.root / f"{name}.stream"
            stream.write_text("tilo-vm-layer-begin " + json.dumps(packed) + "\n"
                              "tilo-vm-layer-data YQ==\n"
                              "tilo-vm-layer-end " + hashlib.sha256(b"a").hexdigest() + " 1\n")
            self.layers.append(dict(component=name, profile="release", features=[],
                               **{key: packed[key] for key in ("target", "toolchain", "source_digest", "layer_hash")},
                               stream=artifacts.file_record(self.root, stream),
                               inventory=artifacts.file_record(self.root, inventory)))
        self.receipt = self.root / "manifest.json"
        self.write()

    def write(self, overrides=None):
        self.receipt.write_text(json.dumps(dict(self.expected, layers=self.layers, **(overrides or {}))))
        self.digest = artifacts.sha(self.receipt)

    def verify(self, expected=None, digest=None):
        return artifacts.verify(self.receipt, expected or self.expected, digest or self.digest,
                                ["shell", "apps+runtime"])

    def test_complete_matching_cohort_orders_prerequisites_first(self):
        self.assertEqual(self.verify(), list(artifacts.COHORT))

    def test_authenticated_receipt_hash_cannot_be_recomputed_from_download(self):
        old = self.digest
        self.receipt.write_text(self.receipt.read_text() + " ")
        with self.assertRaisesRegex(ValueError, "authenticated"):
            self.verify(digest=old)

    def test_stale_cross_run_source_builder_and_architecture_are_rejected(self):
        for key, wrong in dict(run_id=1235, run_attempt=2, source_commit="e" * 40,
                               builder_image=BUILDER.replace("b", "c"), architecture="aarch64").items():
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "mismatch"):
                self.verify(expected=dict(self.expected, **{key: wrong}))

    def test_missing_prerequisite_and_duplicate_layer_rejected(self):
        for layers in (self.layers[1:], [*self.layers, self.layers[0]]):
            with self.subTest(count=len(layers)):
                self.receipt.write_text(json.dumps(dict(self.expected, layers=layers)))
                with self.assertRaisesRegex(ValueError, "incomplete|duplicate"):
                    self.verify(digest=artifacts.sha(self.receipt))

    def test_tampered_stream_size_or_inventory_fails(self):
        for kind in ("stream", "inventory"):
            path = self.root / self.layers[0][kind]["path"]
            saved = path.read_bytes()
            path.write_bytes(saved + b"tamper")
            with self.subTest(kind=kind), self.assertRaisesRegex(ValueError, "bytes/digest"):
                self.verify()
            path.write_bytes(saved)

    def test_forged_header_and_cache_only_and_missing_footer_fail(self):
        path = self.root / self.layers[0]["stream"]["path"]
        saved = path.read_text()
        for content in (saved.replace('"reused": false', '"reused": true'),
                        saved.replace('"component": "launcher"', '"component": "shell"'),
                        saved.rsplit("tilo-vm-layer-end", 1)[0]):
            with self.subTest(content=content[:30]):
                path.write_text(content)
                self.layers[0]["stream"] = artifacts.file_record(self.root, path)
                self.write()
                with self.assertRaises(ValueError):
                    self.verify()

    def test_unknown_fields_and_escaping_paths_fail(self):
        self.write({"untrusted": True})
        with self.assertRaises(ValueError):
            self.verify()
        self.layers[0]["stream"]["path"] = "../escape"
        self.write()
        with self.assertRaisesRegex(ValueError, "escapes"):
            self.verify()

    def handoff(self):
        return dict(receipt=self.receipt, expected=self.expected, sha256=self.digest)

    def test_consumer_uses_only_artifact_command_and_keeps_slot_readback(self):
        class Vm:
            slot = "acceptance"
            binary = "tilo-vm"
            def __init__(self):
                self.calls = []
            def run(self, *args, **kwargs):
                self.calls.append(args)
                if args[1] == "slot":
                    return json.dumps(dict(slot=dict(components={name: "d" * 64 for name in artifacts.COHORT})))
                return json.dumps(dict(layer_hash="d" * 64))
        vm = Vm()
        with patch.object(run, "git", return_value=SOURCE), \
                patch.object(run, "component_scopes", return_value={}):
            result = run.swap_components(vm, ["shell", "apps+runtime"], "ref", self.root, self.handoff())
        self.assertEqual([call[1] for call in vm.calls], [*(["swap-artifact"] * 4), "slot"])
        self.assertEqual([entry["component"] for entry in result["components"]], list(artifacts.COHORT))
        self.assertTrue(all(entry["layer"] == "d" * 64 for entry in result["components"]))
        self.assertEqual(result["commit"], SOURCE)
        self.assertEqual(result["slot_layers"]["acceptance"]["shell"], "d" * 64)
        for call in vm.calls[:-1]:
            self.assertIn(self.digest, call)
            self.assertNotIn("--builder", call)

    def test_named_consumer_runs_every_artifact_swap_in_the_ref_checkout(self):
        def fake(binary):
            vm = unittest.mock.Mock(slot="acceptance", binary=binary)
            vm.run.return_value = json.dumps(dict(slot=dict(components={name: "d" * 64 for name in artifacts.COHORT})))
            return vm
        vm, consumer = fake("ref-tilo-vm"), fake("workflow-tilo-vm")
        with patch.object(run, "git", return_value=SOURCE), \
                patch.object(run, "component_scopes", return_value={}):
            result = run.swap_components(vm, ["apps+runtime"], "ref", self.root,
                                         dict(self.handoff(), consumer=consumer))
        swaps = [call for call in consumer.run.call_args_list if call.args[1] == "swap-artifact"]
        self.assertEqual(len(swaps), 4)
        self.assertTrue(all(call.kwargs["cwd"] == self.root for call in swaps))
        self.assertTrue(all(call.args[1] != "swap-artifact" for call in vm.run.call_args_list))
        self.assertTrue(all(entry["consumer"] == "workflow-tilo-vm" for entry in result["components"]))

    def test_invalid_handoff_fails_before_any_vm_action(self):
        vm = unittest.mock.Mock(slot="acceptance")
        self.receipt.write_text("{}")
        with patch.object(run, "git", return_value=SOURCE), \
                patch.object(run, "component_scopes", return_value={}), self.assertRaises(ValueError):
            run.swap_components(vm, ["apps+runtime"], "ref", self.root, self.handoff())
        vm.run.assert_not_called()

    def test_activation_failure_has_no_build_fallback(self):
        vm = unittest.mock.Mock(slot="acceptance")
        vm.run.side_effect = VmError("activation failed")
        with patch.object(run, "git", return_value=SOURCE), \
                patch.object(run, "component_scopes", return_value={}):
            result = run.swap_components(vm, ["apps+runtime"], "ref", self.root, self.handoff())
        self.assertEqual(len(result["components"]), 4)
        self.assertTrue(all(entry.get("error") for entry in result["components"]))
        self.assertTrue(all(call.args[1] == "swap-artifact" for call in vm.run.call_args_list))

    def producer(self, mutate=False):
        repo = self.root / "source"
        (repo / "os").mkdir(parents=True)
        (repo / "Cargo.lock").write_text("locked fixture")
        (repo / "os/components.toml").write_text("\n".join(
            f'[[component]]\nname = "{name}"\nkind = "userspace"\n'
            'profile = "release"\ndev_features = ["fixture"]\n' for name in artifacts.COHORT))
        calls = []
        def execute(command, **kwargs):
            if command[:2] == ["git", "rev-parse"]:
                return subprocess.CompletedProcess(command, 0, SOURCE + "\n")
            if command[:2] == ["git", "status"]:
                return subprocess.CompletedProcess(command, 0, b"")
            if command[:2] == ["git", "ls-files"]:
                return subprocess.CompletedProcess(command, 0, b"Cargo.lock\0os/components.toml\0")
            calls.append(command)
            name = command[command.index("--component") + 1]
            inventory = Path(command[command.index("--source-inventory") + 1])
            packed = dict(component=name, target="x86_64-unknown-linux-gnu", toolchain="rustc fixture",
                          source_digest=artifacts.sha(inventory), layer_hash="d" * 64,
                          reused=False, binary=False)
            kwargs["stdout"].write(("tilo-vm-layer-begin " + json.dumps(packed) + "\n"
                                    "tilo-vm-layer-data YQ==\n"
                                    "tilo-vm-layer-end " + hashlib.sha256(b"a").hexdigest() + " 1\n").encode())
            if mutate:
                (repo / "Cargo.lock").write_text("changed during assembly")
            return subprocess.CompletedProcess(command, 0)
        with patch.object(artifacts.subprocess, "run", side_effect=execute):
            digest = artifacts.produce(repo, self.root / "produced", self.expected,
                                       ["shell", "apps+runtime"], "maintained-packer")
        return calls, digest

    def test_producer_uses_maintained_packer_and_full_source_inventories(self):
        calls, digest = self.producer()
        self.assertEqual([call[3] for call in calls], list(artifacts.COHORT))
        for call in calls:
            self.assertEqual(call[:2], ["maintained-packer", "layer-pack"])
            self.assertNotIn("--cached-layer", call)
        self.assertEqual(artifacts.verify(self.root / "produced/manifest.json", self.expected,
                         digest, ["shell", "apps+runtime"]), list(artifacts.COHORT))

    def test_producer_rejects_source_changes_during_assembly(self):
        with self.assertRaisesRegex(ValueError, "source changed"):
            self.producer(mutate=True)
        self.assertFalse((self.root / "produced/manifest.json").exists())

    def test_workflow_uses_same_run_artifact_and_authenticated_step_output(self):
        workflow = (HERE.parents[1] / ".forgejo/workflows/vm-acceptance.yml").read_text()
        self.assertIn("actions/upload-artifact@16871d9e8cfcf27ff31822cac382bbb5450f1e1e", workflow)
        self.assertIn("actions/download-artifact@d8d0a99033603453ad2255e58720b460a0555e1e", workflow)
        self.assertEqual(workflow.count("name: vm-cohort-${{ github.run_id }}-${{ github.run_attempt }}-${{ steps.plan.outputs.target }}"), 2)
        self.assertIn("RECEIPT_SHA256: ${{ steps.cohort.outputs.receipt-sha256 }}", workflow)
        self.assertIn('--artifact-receipt-sha256 "$RECEIPT_SHA256"', workflow)
        # The verifier is the workflow's own tilo-vm, never the ref's under test.
        self.assertIn("--artifact-tilo-vm /usr/local/bin/tilo-vm-artifact", workflow)
        self.assertLess(workflow.index("Build the artifact consumer from the workflow checkout"),
                        workflow.index("Produce exact-source signed cohort layers"))
        self.assertIn("if-no-files-found: error", workflow)
        self.assertLess(workflow.index("Verify the same-run cohort handoff"), workflow.index("Prepare the libvirt session user"))
        producer = workflow.split("Produce exact-source signed cohort layers", 1)[1].split("Build the fixture-matching", 1)[0]
        self.assertNotIn("secrets.", producer)
        self.assertNotIn("remote_builders", producer)


if __name__ == "__main__":
    unittest.main()


class BaseEngineTests(unittest.TestCase):
    """apps+runtime ships the engines its apps pin; on a CI host they come from the base."""

    def test_pinned_engines_the_base_has_are_copied_and_others_left_to_the_assembly(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, base, out = root / "src", root / "base", root / "out"
            for app, pins in (("browser", '["tilo-runtime/1.0-chromium"]'),
                              ("mail", '["tilo-runtime/1.0-chromium", "tilo-runtime/2.0-other"]'),
                              ("notes", None)):
                (source / "apps" / app).mkdir(parents=True)
                extra = f"optional-runtimes = {pins}\n" if pins else ""
                (source / "apps" / app / "tilo-app.toml").write_text(
                    f'[package]\nid = "so.tilo.{app}"\n{extra}')
            (base / "1.0-chromium").mkdir(parents=True)
            (base / "1.0-chromium" / "bundle.toml").write_text("signed = true\n")
            self.assertEqual(run.engine_pins(source), ["1.0-chromium", "2.0-other"])

            class Vm:
                binary, vm = "tilo-vm", "acceptance-vm"

                def root_exec(self, script, timeout=120):
                    version = script.split("/usr/lib/tilo-runtime/")[1].split("/")[0]
                    return "present\n" if (base / version / "bundle.toml").is_file() else ""

            real_popen = subprocess.Popen

            def popen(argv, **kwargs):
                if argv[0] != "tilo-vm":
                    return real_popen(argv, **kwargs)
                version = argv[-1].rsplit(" ", 1)[1]
                return real_popen(["tar", "-C", str(base), "-cf", "-", version], **kwargs)

            with patch.object(run.subprocess, "Popen", side_effect=popen):
                staged = run.stage_base_engines(Vm(), source, out)
            self.assertEqual(staged, ["1.0-chromium"])
            self.assertEqual((out / "1.0-chromium" / "bundle.toml").read_text(), "signed = true\n")
            self.assertFalse((out / "2.0-other").exists())
