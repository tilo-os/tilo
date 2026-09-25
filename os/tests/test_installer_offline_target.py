#!/usr/bin/env python3
"""Authentication checks for a separate offline installed image."""

import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "usr/libexec/tilo-installer-offline-target.py"
SPEC = importlib.util.spec_from_file_location("offline_target", SCRIPT)
target = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(target)
SOURCE = "a" * 40
UPDATE = "code.sandbox.example/qos/tilo:lenovo-83ed-" + SOURCE


class OfflineTargetTests(unittest.TestCase):
    def fixture(self, directory):
        root = Path(directory) / "target-oci"
        layout = root / "oci"
        blobs = layout / "blobs/sha256"
        blobs.mkdir(parents=True)
        manifest = b'{"schemaVersion":2,"config":{},"layers":[]}'
        digest = "sha256:" + hashlib.sha256(manifest).hexdigest()
        (blobs / digest[7:]).write_bytes(manifest)
        (layout / "oci-layout").write_text('{"imageLayoutVersion":"1.0.0"}')
        (layout / "index.json").write_text(json.dumps({
            "schemaVersion": 2, "manifests": [{"mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": digest, "size": len(manifest),
                "annotations": {"kind": "dev.cosignproject.cosign/image"}}]}))
        metadata = {"schema": "tilo.installer-offline-target.v1",
                    "target_ref": "code.sandbox.example/qos/tilo@" + digest,
                    "target_digest": digest, "architecture": "aarch64",
                    "hardware_profile": "lenovo-83ed", "update_origin": UPDATE}
        (root / "target.json").write_text(json.dumps(metadata))
        statement = {"predicateType": "https://slsa.dev/provenance/v1",
                     "subject": [{"digest": {"sha256": digest[7:]}}],
                     "predicate": {"buildDefinition": {
                         "buildType": target.BUILD_TYPE,
                         "externalParameters": {"generation": 1, "architecture": "aarch64",
                                                "hardwareProfile": "lenovo-83ed",
                                                "firmwareManifestSha256": "b" * 64},
                         "internalParameters": {"expectedReleaseDigest": digest},
                         "resolvedDependencies": [
                             {"uri": "git+ssh://code.sandbox.example/qos/qos@" + SOURCE},
                             {"uri": "code.sandbox.example/qos/tilo-lenovo-83ed-firmware@sha256:" + "c" * 64}]},
                         "runDetails": {"builder": {"id": target.BUILDER},
                                        "metadata": {"invocationId": "12345"},
                                        "byproducts": [{"name": "independent-rebuild",
                                                        "uri": "urn:tilo:verification:manifest-digest-equal"}]}}}
        signature = json.dumps([{"critical": {
            "identity": {"docker-reference": "code.sandbox.example/qos/tilo"},
            "image": {"docker-manifest-digest": digest}}}])
        attestation = json.dumps([{"payload": base64.b64encode(json.dumps(statement).encode()).decode()}])
        return root, metadata, signature, attestation

    def test_manifest_signature_and_slsa_bind_selected_target(self):
        with tempfile.TemporaryDirectory() as directory:
            root, metadata, signature, attestation = self.fixture(directory)
            key = Path(directory) / "cosign.pub"
            key.write_text("test public key")
            executable = Path(directory) / "cosign"
            executable.write_text("test")
            receipt = Path(directory) / "run/offline-target.json"
            with patch.object(target, "ROOT", root), patch.object(target, "KEY", key), \
                    patch.object(target, "COSIGN", executable), \
                    patch.object(target, "RECEIPT", receipt), \
                    patch.object(target.os, "access", return_value=True), \
                    patch.object(target, "run", side_effect=[signature, attestation]):
                result = target.verify(root)
            self.assertEqual(result["target_digest"], metadata["target_digest"])
            self.assertEqual(result["provenance"]["source_revision"], SOURCE)
            self.assertEqual(json.loads(receipt.read_text())["update_origin"], UPDATE)

    def test_tampered_manifest_or_origin_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root, metadata, _, attestation = self.fixture(directory)
            blob = root / "oci/blobs/sha256" / metadata["target_digest"][7:]
            blob.write_bytes(blob.read_bytes() + b"x")
            with self.assertRaisesRegex(ValueError, "manifest differs"):
                target.read_target(root)
            blob.write_bytes(blob.read_bytes()[:-1])
            metadata["update_origin"] = UPDATE[:-1] + "b"
            with self.assertRaisesRegex(ValueError, "SLSA"):
                target.checked_provenance(attestation, metadata)

    def test_ambiguous_or_unsigned_target_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root, metadata, _, _ = self.fixture(directory)
            index = json.loads((root / "oci/index.json").read_text())
            index["manifests"].append(index["manifests"][0])
            (root / "oci/index.json").write_text(json.dumps(index))
            with self.assertRaisesRegex(ValueError, "unique target"):
                target.read_target(root)
            index["manifests"].pop()
            (root / "oci/index.json").write_text(json.dumps(index))
            key = Path(directory) / "cosign.pub"
            key.write_text("test public key")
            executable = Path(directory) / "cosign"
            executable.write_text("test")
            receipt = Path(directory) / "run/offline-target.json"
            wrong_signature = json.dumps([{"critical": {"image": {
                "docker-manifest-digest": "sha256:" + "f" * 64}}}])
            with patch.object(target, "KEY", key), patch.object(target, "COSIGN", executable), \
                    patch.object(target, "RECEIPT", receipt), \
                    patch.object(target.os, "access", return_value=True), \
                    patch.object(target, "run", return_value=wrong_signature):
                with self.assertRaisesRegex(ValueError, "signature does not bind"):
                    target.verify(root)


if __name__ == "__main__":
    unittest.main()
