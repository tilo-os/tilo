#!/usr/bin/env python3
"""File-backed checks for the guest's installed-image proof."""

import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "usr/libexec/tilo-vm-install-proof.py"
SPEC = importlib.util.spec_from_file_location("vm_install_proof", SCRIPT)
proof = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(proof)
DIGEST = "sha256:" + "a" * 64
COMPOSEFS = "b" * 128
GUIDS = ["00000000-0000-4000-8000-00000000000" + str(i) for i in range(4)]


class ProofTests(unittest.TestCase):
    def fixture(self, directory):
        root = Path(directory) / "root"
        deploy_etc = root / "state/deploy" / COMPOSEFS / "etc"
        deploy_etc.mkdir(parents=True)
        origin = deploy_etc.parent / (COMPOSEFS + ".origin")
        origin.write_text("[image]\nmanifest_digest=" + DIGEST + "\n"
                          "[origin]\ncontainer-image-reference=ostree-unverified-image:docker://registry.tilo.so/tilo/tilo:latest\n",
                          encoding="utf-8")
        efi = Path(directory) / "efi"
        uki = efi / "EFI/Linux/target.efi"
        uki.parent.mkdir(parents=True)
        uki.write_bytes(b"signed-target-uki")
        args = SimpleNamespace(target_root=root, deploy_etc=deploy_etc, efi_root=efi,
                               target_uki=uki, disk=Path("/dev/vda"),
                               efi_partition=Path("/dev/vda5"),
                               boot_partition=Path("/dev/vda6"),
                               root_partition=Path("/dev/vda7"))
        status = {"status": {"booted": {"incompatible": False,
                  "image": {"imageDigest": DIGEST, "architecture": "arm64"},
                  "composefs": {"verity": COMPOSEFS}}}}
        def command(*argv):
            if argv[:2] == ("bootc", "status"):
                return json.dumps(status)
            if argv[:2] == ("/usr/libexec/tilo-uki-cmdline", "show"):
                return "root=tilo-auto composefs=" + COMPOSEFS
            if argv[:4] == ("blkid", "-p", "-s", "PTUUID"):
                return GUIDS[0]
            if argv[:3] == ("blkid", "-s", "PARTUUID"):
                return GUIDS[{"/dev/vda5": 1, "/dev/vda6": 2, "/dev/vda7": 3}[argv[-1].replace("\\", "/")]]
            raise AssertionError(argv)
        return args, status, origin, command

    def test_proof_identity_matching(self):
        with self.subTest(phase="matching_booted_origin_uki_and_disk_identities"):
            with tempfile.TemporaryDirectory() as directory:
                args, _, _, command = self.fixture(directory)
                with patch.object(proof, "command", side_effect=command):
                    result = proof.build_proof(args)
                self.assertEqual(result["schema"], "tilo.vm-install-image-proof.v1")
                self.assertEqual(result["architecture"], "arm64")
                self.assertEqual(result["booted_image_digest"], DIGEST)
                self.assertEqual(result["installed_image_digest"], DIGEST)
                self.assertEqual(result["composefs_digest"], COMPOSEFS)
                self.assertEqual(result["partuuids"]["root"], GUIDS[3])
        with self.subTest(phase="distinct_signed_medium_and_target"):
            with tempfile.TemporaryDirectory() as directory:
                args, status, origin, command = self.fixture(directory)
                status["status"]["booted"]["image"]["imageDigest"] = "sha256:" + "c" * 64
                status["status"]["booted"]["composefs"]["verity"] = "d" * 128
                target_ref = "code.tilo.so/tilo/tilo@" + DIGEST
                update_origin = "code.tilo.so/tilo/tilo:lenovo-83ed-" + "e" * 40
                receipt = Path(directory) / "offline-target.json"
                receipt.write_text(json.dumps({"schema": "tilo.installer-offline-target.v1",
                                               "target_ref": target_ref, "target_digest": DIGEST,
                                               "hardware_profile": "lenovo-83ed",
                                               "update_origin": update_origin}))
                args.offline_target_receipt = receipt
                origin.write_text("[image]\nmanifest_digest=" + DIGEST + "\n"
                                  "[origin]\ncontainer-image-reference=ostree-unverified-image:docker://" + update_origin + "\n")
                with patch.object(proof, "command", side_effect=command):
                    result = proof.build_proof(args)
                self.assertEqual(result["schema"], "tilo.vm-install-image-proof.v2")
                self.assertEqual(result["booted_composefs_digest"], "d" * 128)
                self.assertEqual(result["installed_composefs_digest"], COMPOSEFS)
                self.assertEqual(result["selected_target"]["target_ref"], target_ref)
                self.assertEqual(result["installed_update_origin"],
                                 "ostree-unverified-image:docker://" + update_origin)
                origin.write_text("[image]\nmanifest_digest=" + DIGEST + "\n"
                                  "[origin]\ncontainer-image-reference=ostree-unverified-image:docker://registry.tilo.so/tilo/tilo:latest\n")
                with patch.object(proof, "command", side_effect=command):
                    with self.assertRaisesRegex(ValueError, "signed offline target"):
                        proof.build_proof(args)
        with self.subTest(phase="development_vm_copy_changes_oci_manifest_only"):
            with tempfile.TemporaryDirectory() as directory:
                args, status, _, command = self.fixture(directory)
                status["status"]["booted"]["image"]["imageDigest"] = "sha256:" + "c" * 64
                with patch.object(proof, "command", side_effect=command), \
                        patch.object(proof, "copied_dev_manifest_digest", return_value=DIGEST):
                    result = proof.build_proof(args)
                self.assertEqual(result["schema"], "tilo.vm-install-image-proof.v3")
                self.assertEqual(result["copied_manifest_digest"], DIGEST)
                with patch.object(proof, "command", side_effect=command), \
                        patch.object(proof, "copied_dev_manifest_digest",
                                     return_value="sha256:" + "d" * 64):
                    with self.assertRaisesRegex(ValueError, "VM-local bootc copy"):
                        proof.build_proof(args)
        with self.subTest(phase="backend_emits_proof_only_after_encrypted_finish"):
            backend = (SCRIPT.parent / "tilo-install").read_text(encoding="utf-8")
            prepared = backend.index("VM_INSTALL_PROOF=/run/tilo-installer/vm-install-proof.json")
            finished = backend.index('"$DISK_ENCRYPT" finish "$ENC_LUKS"')
            emitted = backend.index('proof_line="TILO_VM_INSTALL_PROOF_${proof_version}=')
            self.assertLess(prepared, finished)
            self.assertLess(finished, emitted)
            self.assertIn('VM_INSTALL_PROOF_PORT=/dev/virtio-ports/org.tilo.vm-install-proof', backend)
            self.assertIn('systemd-detect-virt --vm --quiet', backend)
            self.assertIn('printf \'%s\\n\' "$proof_line" > "$VM_INSTALL_PROOF_PORT"', backend)

    def test_proof_refusals(self):
        with self.subTest(phase="mismatching_booted_or_installed_image_refused"):
            with tempfile.TemporaryDirectory() as directory:
                args, status, origin, command = self.fixture(directory)
                status["status"]["booted"]["image"]["imageDigest"] = "sha256:" + "c" * 64
                with patch.object(proof, "command", side_effect=command):
                    with self.assertRaisesRegex(ValueError, "development copy proof"):
                        proof.build_proof(args)
                status["status"]["booted"]["image"]["imageDigest"] = DIGEST
                origin.write_text("[image]\nmanifest_digest=sha256:" + "c" * 64 + "\n"
                                  "[origin]\ncontainer-image-reference=ostree-unverified-image:docker://registry.tilo.so/tilo/tilo:latest\n")
                with patch.object(proof, "command", side_effect=command):
                    with self.assertRaisesRegex(ValueError, "development copy proof"):
                        proof.build_proof(args)
        with self.subTest(phase="wrong_uki_composefs_or_deployment_path_refused"):
            with tempfile.TemporaryDirectory() as directory:
                args, _, _, command = self.fixture(directory)
                def wrong_uki(*argv):
                    if argv[:2] == ("/usr/libexec/tilo-uki-cmdline", "show"):
                        return "composefs=" + "c" * 128
                    return command(*argv)
                with patch.object(proof, "command", side_effect=wrong_uki):
                    with self.assertRaisesRegex(ValueError, "target UKI differs"):
                        proof.build_proof(args)
                args.deploy_etc = Path(directory) / "other"
                args.deploy_etc.mkdir()
                with patch.object(proof, "command", side_effect=command):
                    with self.assertRaises(ValueError):
                        proof.build_proof(args)
        with self.subTest(phase="non_arm64_booted_image_refused"):
            with tempfile.TemporaryDirectory() as directory:
                args, status, _, command = self.fixture(directory)
                status["status"]["booted"]["image"]["architecture"] = "amd64"
                with patch.object(proof, "command", side_effect=command):
                    with self.assertRaisesRegex(ValueError, "ARM64"):
                        proof.build_proof(args)


if __name__ == "__main__":
    unittest.main()
