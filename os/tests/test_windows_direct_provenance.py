#!/usr/bin/env python3
"""Digest and guest-proof binding tests for the physical copy gate."""

import hashlib
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "dev/windows-direct-provenance.py"
SPEC = importlib.util.spec_from_file_location("windows_direct_provenance", SCRIPT)
proof = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(proof)


def write(path, value):
    raw = json.dumps(value, separators=(",", ":")).encode()
    path.write_bytes(raw)
    return "sha256:" + hashlib.sha256(raw).hexdigest()


class ProvenanceTests(unittest.TestCase):
    def test_release_child_selection(self):
        with self.subTest(phase="paired_release_rejects_different_ci_source_revisions"):
            image = "registry.tilo.so/tilo/tilo@sha256:" + "a" * 64
            child = "sha256:" + "b" * 64
            args = SimpleNamespace(
                generic_ci_image=image, generic_index_manifest=Path("index.json"),
                generic_child_manifest=Path("child.json"),
                generic_vm_proof_checked=Path("checked.json"),
                generic_medium_identity=Path("generic.json"),
                generic_ci_guest_proof=Path("generic-proof.json"),
                board_medium_identity=Path("board.json"),
                board_ci_guest_proof=Path("board-proof.json"),
                verify_key=Path("cosign.pub"), cosign_command_json=None,
                cosign_key_arg=None)
            signed = {"source_revision": "1" * 40, "generation": 1,
                      "invocation_id": "run-1"}
            with patch.object(proof, "signed_ci_child",
                              return_value=("sha256:" + "a" * 64, child,
                                            [child], "generic-index")), \
                 patch.object(proof, "verify_ci_image", return_value=signed):
                with self.assertRaisesRegex(ValueError, "same source revision"):
                    proof.dual_ci_medium_gate(args, {}, child, "2" * 40)
        with self.subTest(phase="index_selects_only_signed_arm64_child"):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                child, digest, index, pinned, _ = self.fixture(root)
                self.assertEqual(proof.signed_ci_child("registry.tilo.so/tilo/tilo@" + pinned,
                                                       index, child)[:2], (pinned, digest))
        with self.subTest(phase="duplicate_or_swapped_child_is_refused"):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                child, digest, index, pinned, doc = self.fixture(root)
                doc["manifests"].append(dict(doc["manifests"][0]))
                new_pin = write(index, doc)
                with self.assertRaisesRegex(ValueError, "exactly two"):
                    proof.signed_ci_child("registry.tilo.so/tilo/tilo@" + new_pin, index, child)
                doc["manifests"].pop()
                write(index, doc)
                child.write_bytes(child.read_bytes() + b" ")
                with self.assertRaisesRegex(ValueError, "child manifest"):
                    proof.signed_ci_child("registry.tilo.so/tilo/tilo@" + pinned, index, child)
        with self.subTest(phase="guest_image_and_gpt_must_match_selected_child"):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                _, child, _, _, _ = self.fixture(root)
                ids = {"efi": "00000000-0000-4000-8000-000000000001",
                       "boot": "00000000-0000-4000-8000-000000000002",
                       "root": "00000000-0000-4000-8000-000000000003"}
                guest = {"schema": "tilo.vm-install-image-proof.v1",
                         "architecture": "arm64", "booted_image_digest": child,
                         "installed_image_digest": child, "composefs_digest": "b" * 128,
                         "target_uki_sha256": "a" * 64,
                         "disk_guid": "00000000-0000-4000-8000-000000000004",
                         "partuuids": ids}
                source = {"schema": "tilo.windows-direct-vm-template-check.v1",
                          "preserved_gpt_verified": True,
                          "preserved_edge_samples_verified": True,
                          "final_partition_count": 3,
                          "disk_guid": guest["disk_guid"], "final_partuuids": dict(ids)}
                artifact = {"schema": proof.PROOF_SCHEMA, "guest_proof": guest,
                            "source_check": source, "installer_composefs": "b" * 128}
                path = root / "checked.json"
                path.write_text(json.dumps(artifact))
                self.assertEqual(proof.checked_guest_proof(path, child), artifact)
                guest["installed_image_digest"] = "sha256:" + "e" * 64
                path.write_text(json.dumps(artifact))
                with self.assertRaisesRegex(ValueError, "differs from signed"):
                    proof.checked_guest_proof(path, child)
                guest["installed_image_digest"] = child
                source["final_partuuids"]["root"] = ids["efi"]
                path.write_text(json.dumps(artifact))
                with self.assertRaisesRegex(ValueError, "source GPT"):
                    proof.checked_guest_proof(path, child)

    def fixture(self, root):
        child_path = root / "child.json"
        child = {"schemaVersion": 2, "mediaType": proof.IMAGE,
                 "config": {"digest": "sha256:" + "c" * 64},
                 "layers": [{"digest": "sha256:" + "d" * 64}]}
        child_digest = write(child_path, child)
        index_path = root / "index.json"
        index = {"schemaVersion": 2, "mediaType": proof.INDEX,
                 "manifests": [{"mediaType": proof.IMAGE, "digest": child_digest,
                                "platform": {"os": "linux", "architecture": "arm64"}},
                               {"mediaType": proof.IMAGE, "digest": "sha256:" + "f" * 64,
                                "platform": {"os": "linux", "architecture": "amd64"}}]}
        index_digest = write(index_path, index)
        return child_path, child_digest, index_path, index_digest, index

    def test_signed_target_and_runner(self):
        with self.subTest(phase="v2_signed_target_may_differ_from_booted_installer"):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                target = "sha256:" + "a" * 64
                image = "code.tilo.so/tilo/tilo@" + target
                revision = "b" * 40
                origin = "code.tilo.so/tilo/tilo:lenovo-83ed-" + revision
                ci = {"source_revision": revision, "firmware_manifest_sha256": "c" * 64,
                      "firmware_candidate": "code.tilo.so/tilo/tilo-lenovo-83ed-firmware@sha256:" + "e" * 64,
                      "invocation_id": "234", "key_sha256": "d" * 64,
                      "image_kind": "lenovo-83ed", "generation": 1}
                ids = {"efi": "00000000-0000-4000-8000-000000000001",
                       "boot": "00000000-0000-4000-8000-000000000002",
                       "root": "00000000-0000-4000-8000-000000000003"}
                disk = "00000000-0000-4000-8000-000000000004"
                guest = {"schema": "tilo.vm-install-image-proof.v2", "architecture": "arm64",
                         "booted_image_digest": "sha256:" + "e" * 64,
                         "installed_image_digest": target,
                         "booted_composefs_digest": "f" * 128,
                         "installed_composefs_digest": "a" * 128,
                         "composefs_digest": "a" * 128,
                         "installed_update_origin": "ostree-unverified-image:docker://" + origin,
                         "target_uki_sha256": "b" * 64, "disk_guid": disk, "partuuids": ids,
                         "selected_target": {"schema": "tilo.installer-offline-target.v1",
                             "target_ref": image, "target_digest": target,
                             "architecture": "aarch64", "hardware_profile": "lenovo-83ed",
                             "update_origin": origin,
                             "provenance": {"source_revision": revision,
                                 "firmware_manifest_sha256": "c" * 64,
                                 "firmware_candidate": ci["firmware_candidate"],
                                 "invocation_id": "234"},
                             "verifier_key_sha256": "d" * 64}}
                artifact = {"schema": proof.PROOF_SCHEMA_V2,
                            "first_boot_verified": False, "guest_proof": guest,
                            "installer_medium_sha256": "9" * 64,
                            "installer_composefs": "f" * 128,
                            "source_check": {"schema": "tilo.windows-direct-vm-template-check.v1",
                                "preserved_gpt_verified": True,
                                "preserved_edge_samples_verified": True,
                                "final_partition_count": 3, "disk_guid": disk,
                                "final_partuuids": ids}}
                path = root / "checked.json"
                path.write_text(json.dumps(artifact))
                self.assertEqual(proof.checked_guest_proof(path, target, image, ci), artifact)
                for name, change in (
                    ("origin", lambda: guest.update(installed_update_origin="local")),
                    ("source", lambda: guest["selected_target"]["provenance"].update(source_revision="f" * 40)),
                    ("firmware candidate", lambda: guest["selected_target"]["provenance"].update(firmware_candidate="local")),
                    ("installer composefs", lambda: artifact.update(installer_composefs="a" * 128))):
                    with self.subTest(name=name):
                        broken = copy.deepcopy(artifact)
                        path.write_text(json.dumps(broken))
                        change()
                        path.write_text(json.dumps(artifact))
                        with self.assertRaises(ValueError):
                            proof.checked_guest_proof(path, target, image, ci)
                        artifact = broken
                        guest = artifact["guest_proof"]
                generic = copy.deepcopy(artifact)
                generic["first_boot_verified"] = True
                selected = generic["guest_proof"]["selected_target"]
                selected["hardware_profile"] = "generic"
                selected["target_ref"] = "registry.tilo.so/tilo/tilo@" + target
                selected["update_origin"] = "registry.tilo.so/tilo/tilo:gen7"
                selected["provenance"].update(generation=7, firmware_candidate=None,
                                              firmware_manifest_sha256=None)
                generic["guest_proof"]["installed_update_origin"] = (
                    "ostree-unverified-image:docker://registry.tilo.so/tilo/tilo:gen7")
                generic_ci = dict(ci, image_kind="generic-child", generation=7,
                                  firmware_candidate=None, firmware_manifest_sha256=None)
                path.write_text(json.dumps(generic))
                self.assertEqual(proof.checked_guest_proof(
                    path, target, "registry.tilo.so/tilo/tilo@sha256:" + "e" * 64,
                    generic_ci), generic)
        with self.subTest(phase="cosign_runner_uses_argv_prefix_and_explicit_wsl_key_path"):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                command = root / "runner.json"
                command.write_text(json.dumps(["wsl.exe", "-d", "FedoraLinux-42",
                                               "--exec", "/home/user/bin/cosign"]))
                key = root / "cosign.pub"
                key.write_bytes(b"public-key")
                on_windows = proof.platform.system() == "Windows"
                wsl_key = (("/mnt/" + key.resolve().drive[0].lower() + "/"
                            + "/".join(key.resolve().parts[1:])) if on_windows else str(key))
                image = "registry.tilo.so/tilo/tilo@sha256:" + "a" * 64
                calls = []
                children = [{"architecture":"aarch64","digest":"sha256:"+"b"*64},
                            {"architecture":"x86_64","digest":"sha256:"+"c"*64}]
                statement = {"predicateType":"https://slsa.dev/provenance/v1",
                             "subject":[{"name":"tilo","digest":{"sha256":"a"*64}}],
                             "predicate":{"buildDefinition":{"buildType":proof.BUILD_TYPE,
                                 "externalParameters":{"children":list(reversed(children)),"generation":1},
                                 "internalParameters":{"expectedReleaseDigest":"sha256:"+"a"*64},
                                 "resolvedDependencies":[{"uri":"git+ssh://code.tilo.so/tilo/tilo@"+"d"*40}]},
                                 "runDetails":{"builder":{"id":proof.BUILDER_ID},
                                 "metadata":{"invocationId":"1234"},
                                 "byproducts":[{"name":"independent-rebuild",
                                 "uri":"urn:tilo:verification:manifest-digest-equal"}]}}}
                signed = json.dumps([{"critical":{"image":{"docker-manifest-digest":"sha256:"+"a"*64}}}])
                attested = json.dumps([{"payload":proof.base64.b64encode(
                    json.dumps(statement).encode()).decode()}])
                def run(argv, **_kwargs):
                    calls.append(argv)
                    return SimpleNamespace(returncode=0, stderr="",
                                           stdout=signed if "verify-attestation" not in argv else attested)
                with patch.object(proof.subprocess, "run", side_effect=run):
                    result = proof.verify_ci_image(
                        image, key, children, "generic-index", command, wsl_key if on_windows else None)
                self.assertEqual(result["key_sha256"], hashlib.sha256(b"public-key").hexdigest())
                self.assertEqual(result["source_revision"], "d"*40)
                self.assertEqual(len(calls), 2)
                self.assertEqual(calls[0][:5], ["wsl.exe", "-d", "FedoraLinux-42",
                                                 "--exec", "/home/user/bin/cosign"])
                self.assertEqual(calls[0][5:8], ["verify", "--key", wsl_key])
                if on_windows:
                    with self.assertRaisesRegex(ValueError, "differs from the native"):
                        proof.verify_ci_image(image, key, children, "generic-index", command, "/mnt/c/other.pub")
        with self.subTest(phase="signed_medium_identity_and_paired_host_proofs"):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                key = root / "cosign.pub"
                key.write_bytes(b"key")
                image = "code.tilo.so/tilo/tilo@sha256:" + "a" * 64
                base = "registry.tilo.so/tilo/tilo@sha256:" + "b" * 64
                native = {"schema": "tilo.vm-install-image-proof.v2", "architecture": "arm64",
                          "installed_image_digest": image.split("@", 1)[1],
                          "selected_target": {"schema": "tilo.installer-offline-target.v1",
                                              "target_ref": image,
                                              "target_digest": image.split("@", 1)[1],
                                              "hardware_profile": "lenovo-83ed"}}
                ci_proof = root / "native.json"
                ci_proof.write_text(json.dumps(native))
                identity = {"schema": "tilo.whpx-ci-installer-medium.v1",
                            "target_profile": "lenovo-83ed", "target_image": image,
                            "live_base_image": base, "build_revision": "c" * 40,
                            "run_id": "12", "run_attempt": "1", "qcow2_sha256": "d" * 64,
                            "archive_sha256": "e" * 64,
                            "guest_proof_sha256": hashlib.sha256(ci_proof.read_bytes()).hexdigest()}
                path = root / "medium.whpx-source.json"
                path.write_text(json.dumps(identity))
                Path(str(path) + ".sig").write_text("signature")
                Path(str(path) + ".bundle").write_text("bundle")
                with patch.object(proof.subprocess, "run", return_value=SimpleNamespace(
                        returncode=0, stderr="", stdout="")) as runner:
                    board = proof.signed_medium_identity(path, ci_proof, key)
                    self.assertIn("verify-blob", runner.call_args.args[0])
                generic = dict(board, target_profile="generic", target_image=base,
                               qcow2_sha256="f" * 64)
                updated = {**identity, "schema": "tilo.whpx-ci-installer-medium.v2",
                           "os_source_revision": "c" * 40, "build_revision": "8" * 40}
                path.write_text(json.dumps(updated))
                with patch.object(proof.subprocess, "run", return_value=SimpleNamespace(
                        returncode=0, stderr="", stdout="")):
                    updated_board = proof.signed_medium_identity(path, ci_proof, key)
                    for revision in (None, "invalid"):
                        path.write_text(json.dumps({**updated, "os_source_revision": revision}))
                        with self.assertRaisesRegex(ValueError, "unexpected fields"):
                            proof.signed_medium_identity(path, ci_proof, key)
                updated_generic = {**generic, "build_revision": "8" * 40,
                                   "os_source_revision": "c" * 40}
                pair_args = ({"first_boot_verified": False,
                              "installer_medium_sha256": "d" * 64},
                             {"first_boot_verified": True,
                              "installer_medium_sha256": "f" * 64},
                             image.split("@", 1)[1], base.split("@", 1)[1], base, "c" * 40)
                proof.checked_medium_pair(updated_board, updated_generic, *pair_args)
                with self.assertRaisesRegex(ValueError, "source revision differs"):
                    proof.checked_medium_pair(updated_board,
                                              {**updated_generic, "os_source_revision": "9" * 40},
                                              *pair_args)
                with self.assertRaisesRegex(ValueError, "paired WHPX"):
                    proof.checked_medium_pair(updated_board,
                                              {**updated_generic, "build_revision": "7" * 40},
                                              *pair_args)
                path.write_text(json.dumps(identity))
                proof.checked_medium_pair(board, generic,
                                          {"first_boot_verified": False,
                                           "installer_medium_sha256": "d" * 64},
                                          {"first_boot_verified": True,
                                           "installer_medium_sha256": "f" * 64},
                                          image.split("@", 1)[1], base.split("@", 1)[1], base,
                                          "c" * 40)
                with self.assertRaisesRegex(ValueError, "source revision differs"):
                    proof.checked_medium_pair(board, generic,
                                              {"first_boot_verified": False,
                                               "installer_medium_sha256": "d" * 64},
                                              {"first_boot_verified": True,
                                               "installer_medium_sha256": "f" * 64},
                                              image.split("@", 1)[1], base.split("@", 1)[1], base,
                                              "9" * 40)
                for field, other in (("run_id", "13"), ("run_attempt", "2")):
                    with self.subTest(field=field), self.assertRaisesRegex(ValueError, "paired WHPX"):
                        proof.checked_medium_pair(board, {**generic, field: other},
                                                  {"first_boot_verified": False,
                                                   "installer_medium_sha256": "d" * 64},
                                                  {"first_boot_verified": True,
                                                   "installer_medium_sha256": "f" * 64},
                                                  image.split("@", 1)[1], base.split("@", 1)[1], base,
                                                  "c" * 40)
                with self.assertRaisesRegex(ValueError, "paired WHPX"):
                    proof.checked_medium_pair(board, generic,
                                              {"first_boot_verified": False,
                                               "installer_medium_sha256": "0" * 64},
                                              {"first_boot_verified": True,
                                               "installer_medium_sha256": "f" * 64},
                                              image.split("@", 1)[1], base.split("@", 1)[1], base,
                                              "c" * 40)
                ci_proof.write_text(json.dumps({**native, "architecture": "amd64"}))
                with patch.object(proof.subprocess, "run", return_value=SimpleNamespace(
                        returncode=0, stderr="", stdout="")):
                    with self.assertRaisesRegex(ValueError, "native guest proof differs"):
                        proof.signed_medium_identity(path, ci_proof, key)

    def test_slsa_predicates(self):
        with self.subTest(phase="slsa_rejects_wrong_subject_builder_source_and_generation"):
            pinned = "sha256:" + "a" * 64
            children = [{"architecture": "aarch64", "digest": "sha256:" + "b" * 64},
                        {"architecture": "x86_64", "digest": "sha256:" + "c" * 64}]
            statement = {"predicateType": "https://slsa.dev/provenance/v1",
                         "subject": [{"digest": {"sha256": "a" * 64}}],
                         "predicate": {"buildDefinition": {
                             "buildType": proof.BUILD_TYPE,
                             "externalParameters": {"children": children, "generation": 5},
                             "internalParameters": {"expectedReleaseDigest": pinned},
                             "resolvedDependencies": [{"uri":
                                 "git+ssh://code.tilo.so/tilo/tilo@" + "d" * 40}]},
                             "runDetails": {"builder": {"id": proof.BUILDER_ID},
                                            "metadata": {"invocationId": "123"},
                                            "byproducts": [{"name": "independent-rebuild",
                                                "uri": "urn:tilo:verification:manifest-digest-equal"}]}}}
            def wrapped(value):
                return json.dumps({"payload": proof.base64.b64encode(
                    json.dumps(value).encode()).decode()})
            self.assertEqual(proof.checked_slsa_attestation(wrapped(statement), pinned, children, "generic-index")
                             ["generation"], 5)
            mutations = [
                ("subject", lambda doc: doc["subject"][0]["digest"].update(sha256="e" * 64)),
                ("builder", lambda doc: doc["predicate"]["runDetails"]["builder"].update(id="local")),
                ("source", lambda doc: doc["predicate"]["buildDefinition"]["resolvedDependencies"][0]
                 .update(uri="git+ssh://other/repo@" + "d" * 40)),
                ("generation", lambda doc: doc["predicate"]["buildDefinition"]["externalParameters"]
                 .update(generation=0)),
                ("children", lambda doc: doc["predicate"]["buildDefinition"]["externalParameters"]
                 .update(children=[])),
            ]
            for name, mutate in mutations:
                with self.subTest(name=name):
                    bad = copy.deepcopy(statement)
                    mutate(bad)
                    with self.assertRaisesRegex(ValueError, "does not uniquely bind"):
                        proof.checked_slsa_attestation(wrapped(bad), pinned, children, "generic-index")
        with self.subTest(phase="lenovo_single_manifest_and_board_predicate"):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                child, digest, _, _, _ = self.fixture(root)
                image = "code.tilo.so/tilo/tilo@" + digest
                self.assertEqual(proof.signed_ci_child(image, child, child),
                                 (digest, digest, [], "lenovo-83ed"))
                # A board image carries private firmware: never the public repository.
                with self.assertRaisesRegex(ValueError, "official repository"):
                    proof.signed_ci_child("registry.tilo.so/tilo/tilo@" + digest, child, child)
            candidate = "code.tilo.so/tilo/tilo-lenovo-83ed-firmware@sha256:" + "e" * 64
            statement = {"predicateType": "https://slsa.dev/provenance/v1",
                         "subject": [{"digest": {"sha256": digest[7:]}}],
                         "predicate": {"buildDefinition": {
                             "buildType": proof.BUILD_TYPE,
                             "externalParameters": {"generation": 1,
                                 "hardwareProfile": "lenovo-83ed", "architecture": "aarch64",
                                 "firmwareManifestSha256": "f" * 64},
                             "internalParameters": {"expectedReleaseDigest": digest},
                             "resolvedDependencies": [
                                 {"uri": "git+ssh://code.tilo.so/tilo/tilo@" + "d" * 40},
                                 {"uri": candidate}]},
                             "runDetails": {"builder": {"id": proof.BUILDER_ID},
                                 "metadata": {"invocationId": "123"},
                                 "byproducts": [{"name": "independent-rebuild",
                                     "uri": "urn:tilo:verification:manifest-digest-equal"}]}}}
            def wrapped(value):
                return json.dumps({"payload": proof.base64.b64encode(
                    json.dumps(value).encode()).decode()})
            good = proof.checked_slsa_attestation(wrapped(statement), digest, [], "lenovo-83ed")
            self.assertEqual(good["firmware_candidate"], candidate)
            for name, change in (
                ("profile", lambda d: d["predicate"]["buildDefinition"]["externalParameters"].update(hardwareProfile="other")),
                ("architecture", lambda d: d["predicate"]["buildDefinition"]["externalParameters"].update(architecture="x86_64")),
                ("firmware", lambda d: d["predicate"]["buildDefinition"]["resolvedDependencies"][-1].update(uri="local")),
                ("generation", lambda d: d["predicate"]["buildDefinition"]["externalParameters"].update(generation=2))):
                with self.subTest(name=name):
                    bad = copy.deepcopy(statement)
                    change(bad)
                    with self.assertRaisesRegex(ValueError, "does not uniquely bind"):
                        proof.checked_slsa_attestation(wrapped(bad), digest, [], "lenovo-83ed")
        with self.subTest(phase="generic_child_requires_its_own_signed_slsa"):
            pinned = "sha256:" + "a" * 64
            statement = {"predicateType": "https://slsa.dev/provenance/v1",
                         "subject": [{"digest": {"sha256": "a" * 64}}],
                         "predicate": {"buildDefinition": {
                             "buildType": proof.BUILD_TYPE,
                             "externalParameters": {"generation": 7},
                             "internalParameters": {"expectedReleaseDigest": pinned},
                             "resolvedDependencies": [{"uri":
                                 "git+ssh://code.tilo.so/tilo/tilo@" + "b" * 40}]},
                             "runDetails": {"builder": {"id": proof.BUILDER_ID},
                                 "metadata": {"invocationId": "321"},
                                 "byproducts": [{"name": "independent-rebuild",
                                     "uri": "urn:tilo:verification:manifest-digest-equal"}]}}}
            def wrapped(value):
                return json.dumps({"payload": proof.base64.b64encode(
                    json.dumps(value).encode()).decode()})
            self.assertEqual(proof.checked_slsa_attestation(
                wrapped(statement), pinned, [], "generic-child")["generation"], 7)
            for field in ("children", "hardwareProfile", "architecture"):
                broken = copy.deepcopy(statement)
                broken["predicate"]["buildDefinition"]["externalParameters"][field] = []
                with self.assertRaises(ValueError):
                    proof.checked_slsa_attestation(wrapped(broken), pinned, [], "generic-child")


if __name__ == "__main__":
    unittest.main()
