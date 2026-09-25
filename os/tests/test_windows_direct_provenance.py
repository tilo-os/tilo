#!/usr/bin/env python3
"""Digest and guest-proof binding tests for the physical copy gate."""

import hashlib
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
    def fixture(self, root):
        child_path = root / "child.json"
        child = {"schemaVersion": 2, "mediaType": proof.IMAGE,
                 "config": {"digest": "sha256:" + "c" * 64},
                 "layers": [{"digest": "sha256:" + "d" * 64}]}
        child_digest = write(child_path, child)
        index_path = root / "index.json"
        index = {"schemaVersion": 2, "mediaType": proof.INDEX,
                 "manifests": [{"mediaType": proof.IMAGE, "digest": child_digest,
                                "platform": {"os": "linux", "architecture": "arm64"}}]}
        index_digest = write(index_path, index)
        return child_path, child_digest, index_path, index_digest, index

    def test_index_selects_only_signed_arm64_child(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            child, digest, index, pinned, _ = self.fixture(root)
            self.assertEqual(proof.signed_ci_child("example/tilo@" + pinned, index, child),
                             (pinned, digest))

    def test_duplicate_or_swapped_child_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            child, digest, index, pinned, doc = self.fixture(root)
            doc["manifests"].append(dict(doc["manifests"][0]))
            new_pin = write(index, doc)
            with self.assertRaisesRegex(ValueError, "exactly one"):
                proof.signed_ci_child("example/tilo@" + new_pin, index, child)
            doc["manifests"].pop()
            write(index, doc)
            child.write_bytes(child.read_bytes() + b" ")
            with self.assertRaisesRegex(ValueError, "child manifest"):
                proof.signed_ci_child("example/tilo@" + pinned, index, child)

    def test_guest_image_and_gpt_must_match_selected_child(self):
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

    def test_cosign_runner_uses_argv_prefix_and_explicit_wsl_key_path(self):
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
            image = "example/tilo@sha256:" + "a" * 64
            calls = []
            def run(argv, **_kwargs):
                calls.append(argv)
                return SimpleNamespace(returncode=0, stderr="")
            with patch.object(proof.subprocess, "run", side_effect=run):
                self.assertEqual(proof.verify_ci_image(
                    image, key, command, wsl_key if on_windows else None),
                    hashlib.sha256(b"public-key").hexdigest())
            self.assertEqual(len(calls), 2)
            self.assertEqual(calls[0][:5], ["wsl.exe", "-d", "FedoraLinux-42",
                                             "--exec", "/home/user/bin/cosign"])
            self.assertEqual(calls[0][5:8], ["verify", "--key", wsl_key])
            if on_windows:
                with self.assertRaisesRegex(ValueError, "differs from the native"):
                    proof.verify_ci_image(image, key, command, "/mnt/c/other.pub")


if __name__ == "__main__":
    unittest.main()
