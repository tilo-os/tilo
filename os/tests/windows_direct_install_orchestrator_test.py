"""Contract checks for the Windows direct-install orchestration boundary."""

import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
import urllib.request
import zipfile
from unittest import mock
from types import SimpleNamespace


SCRIPT = Path(__file__).resolve().parents[1] / "dev/windows-direct-install.py"
SPEC = importlib.util.spec_from_file_location("windows_direct_install", SCRIPT)
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)


class OrchestratorContract(unittest.TestCase):
    def test_medium_binds_actual_imager_checkout_and_pinned_os_separately(self):
        data = {"verify_key": Path("key"), "cosign_command_json": None,
                "cosign_key_arg": None, "source_revision": "a" * 40,
                "generic_image": module.PUBLIC + "b" * 64}
        identity = {"run_id": "9001", "build_revision": "c" * 40,
                    "os_source_revision": "a" * 40, "live_base_image": data["generic_image"]}
        verifier = SimpleNamespace(signed_medium_identity=mock.Mock(return_value=identity))
        spec = SimpleNamespace(loader=SimpleNamespace(exec_module=lambda _: None))
        with mock.patch.object(module.importlib.util, "spec_from_file_location", return_value=spec), \
                mock.patch.object(module.importlib.util, "module_from_spec", return_value=verifier):
            self.assertEqual(module.verify_identity(data, Path("work"), "generic", 9001,
                                                    "c" * 40), identity)
            for field in ("build_revision", "os_source_revision", "run_id"):
                verifier.signed_medium_identity.return_value = {**identity, field: "9" * 40}
                with self.subTest(field=field), self.assertRaisesRegex(ValueError, "pinned CI"):
                    module.verify_identity(data, Path("work"), "generic", 9001, "c" * 40)

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.work = Path(self.temporary.name)
        self.inputs = {}
        for key in ("realized_plan", "generic_index_manifest", "generic_child_manifest",
                    "lenovo_child_manifest", "verify_key", "cosign_command_json", "tilo_vm_exe"):
            path = self.work / key
            path.write_text(key, encoding="utf-8")
            self.inputs[key] = str(path)
        self.inputs.update({
            "schema": "tilo.windows-direct-install.v1",
            "source_revision": "a" * 40,
            "imager_run": 101, "generic_os_run": 102, "lenovo_os_run": 103,
            "generic_image": module.PUBLIC + "b" * 64,
            "lenovo_image": module.BOARD + "c" * 64,
            "source_raw": str(self.work / "source.raw"),
            "cosign_key_arg": "/mnt/c/keys/cosign.pub",
            "username": "installer",
        })
        self.config = self.work / "input.json"
        self.config.write_text(json.dumps(self.inputs), encoding="utf-8")

    def test_artifact_download_streams_and_retains_only_complete_zip(self):
        archive = io.BytesIO()
        with zipfile.ZipFile(archive, "w") as bundle:
            bundle.writestr("proof.json", "{}")
        payload = archive.getvalue()

        class BoundedResponse(io.BytesIO):
            def read(self, size=-1):
                if not 0 < size <= 1024 * 1024:
                    raise AssertionError("artifact download must use bounded reads")
                return super().read(size)

        destination = self.work / "artifact.zip"
        metadata = [{"id": 17, "name": "proof", "expired": False}]
        with mock.patch.object(module, "fgj_run", return_value={"id": 9001}), \
                mock.patch.object(module, "command", side_effect=[json.dumps(metadata).encode(), b"test-token\n"]), \
                mock.patch.object(module.urllib.request, "build_opener") as opener:
            opener.return_value.open.return_value = BoundedResponse(payload)
            module.download_artifact(101, "proof", destination)
            request = opener.return_value.open.call_args.args[0]
            self.assertEqual(request.full_url,
                             "https://code.tilo.so/api/v1/repos/tilo/tilo/actions/artifacts/17/zip")
        self.assertEqual(destination.read_bytes(), payload)
        self.assertFalse(destination.with_suffix(".zip.part").exists())

    def test_artifact_redirect_does_not_forward_forgejo_credentials(self):
        request = urllib.request.Request("https://code.tilo.so/archive",
                                         headers={"Authorization": "token secret"})
        redirected = module.ArtifactRedirect().redirect_request(
            request, None, 302, "Found", {}, "https://storage.example/signed")
        self.assertIsNone(redirected.get_header("Authorization"))
        with self.assertRaisesRegex(ValueError, "HTTPS"):
            module.ArtifactRedirect().redirect_request(
                request, None, 302, "Found", {}, "http://storage.example/signed")

    def test_input_rejects_mutable_image_and_relative_source(self):
        data = module.pinned_config(self.config)
        self.assertEqual(data["source_revision"], "a" * 40)
        self.inputs["lenovo_image"] = "code.tilo.so/tilo/tilo:latest"
        self.config.write_text(json.dumps(self.inputs), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "pinned lenovo_image"):
            module.pinned_config(self.config)
        self.inputs["lenovo_image"] = module.BOARD + "c" * 64
        self.inputs["source_raw"] = "source.raw"
        self.config.write_text(json.dumps(self.inputs), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "source_raw must be an absolute path"):
            module.pinned_config(self.config)

    def test_progress_rejects_changed_inputs_and_outputs(self):
        output = self.work / "proof.json"
        output.write_text("first", encoding="utf-8")
        progress = module.Progress(self.work, "a" * 64)
        progress.mark("proof", [output])
        self.assertTrue(module.Progress(self.work, "a" * 64).done("proof", [output]))
        output.write_text("changed", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "output changed"):
            progress.done("proof", [output])
        with self.assertRaisesRegex(ValueError, "different inputs"):
            module.Progress(self.work, "b" * 64)

    def test_receipt_arguments_bind_both_media_and_proofs(self):
        data = module.pinned_config(self.config)
        for profile in module.PROFILES:
            path = self.work / profile / "scenario" / "vm-install-proof-checked.json"
            path.parent.mkdir(parents=True)
            path.write_text(json.dumps({"schema": "tilo.windows-vm-install-proof-check.v2"}),
                            encoding="utf-8")
        args = [str(value) for value in module.common_evidence(data, self.work)]
        for flag in ("--board-medium-identity", "--board-ci-guest-proof",
                     "--generic-medium-identity", "--generic-ci-guest-proof",
                     "--generic-vm-proof-checked", "--vm-proof-checked",
                     "--cosign-command-json", "--cosign-key-arg"):
            self.assertEqual(args.count(flag), 1)
        self.assertEqual(args[args.index("--ci-image") + 1], data["lenovo_image"])
        self.assertEqual(args[args.index("--generic-ci-image") + 1], data["generic_image"])

    def test_paired_scenarios_use_separate_raw_targets(self):
        data = module.pinned_config(self.config)
        generic = module.scenario_argv(data, self.work, "generic",
                                       self.work / "generic" / "generic-source.raw",
                                       self.work / "generic" / "scenario")
        board = module.scenario_argv(data, self.work, "lenovo-83ed",
                                     data["source_raw"], self.work / "lenovo-83ed" / "scenario")
        self.assertEqual(generic[generic.index("--target-disk") + 1],
                         str(self.work / "generic" / "generic-source.raw"))
        self.assertNotIn("--skip-target-boot", generic)
        self.assertEqual(generic[generic.index("--filesystem") + 1], "ext4")
        self.assertIn("--credentials-stdin", generic)
        self.assertEqual(board[board.index("--target-disk") + 1], str(data["source_raw"]))
        self.assertIn("--skip-target-boot", board)
        self.assertIn("--credentials-stdin", board)

    def test_existing_empty_template_must_match_realized_plan(self):
        preserved = [{"number": 1, "partuuid": "p", "offset_bytes": 4096}]
        targets = [{"role": "efi", "type_guid": "g", "offset_bytes": 8192,
                    "size_bytes": 1024, "partuuid": "q"}]
        plan = {"windows_disk": {"size_bytes": 16384}, "target_partitions": targets}
        manifest = {"disk_size_bytes": 16384, "preserved_partitions": preserved,
                    "target_partitions": [{key: targets[0][key] for key in
                                           ("role", "type_guid", "offset_bytes", "size_bytes")}]}
        self.assertTrue(module.template_matches_plan(manifest, plan, preserved))
        manifest["target_partitions"][0]["offset_bytes"] += 512
        self.assertFalse(module.template_matches_plan(manifest, plan, preserved))

    def test_completed_install_resumes_but_incomplete_record_is_refused(self):
        data = module.pinned_config(self.config)
        out = self.work / "generic" / "scenario"
        out.mkdir(parents=True)
        record = out / "install.json"
        record.write_text(json.dumps({"steps": ["install", "power-off"]}))
        args = module.scenario_argv(data, self.work, "generic", data["source_raw"], out)
        self.assertIn("--resume", args)
        self.assertIn("--keep", args)
        record.write_text(json.dumps({"steps": ["install"]}))
        with self.assertRaisesRegex(ValueError, "did not complete"):
            module.scenario_argv(data, self.work, "generic", data["source_raw"], out)

    def test_forgejo_run_must_be_successful_and_host_pinned(self):
        response = {"run": {"index_in_repo": 101, "id": 9001, "status": "success"}}
        with mock.patch.object(module, "command", return_value=json.dumps(response)) as invoked:
            self.assertEqual(module.fgj_run(101)["id"], 9001)
        argv = invoked.call_args.args[0]
        self.assertEqual(argv[:3], ["fgj", "--hostname", "code.tilo.so"])
        self.assertIn("-R", argv)
        response["run"]["status"] = "waiting"
        with mock.patch.object(module, "command", return_value=json.dumps(response)):
            with self.assertRaisesRegex(ValueError, "not completed successfully"):
                module.fgj_run(101)


if __name__ == "__main__":
    unittest.main()
