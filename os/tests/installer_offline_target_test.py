"""Synthetic OCI transport checks for the CI-preloaded installer target."""

import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

MODULE = Path(__file__).resolve().parents[1] / "dev/installer-offline-target.py"
spec = importlib.util.spec_from_file_location("installer_offline_target", MODULE)
offline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(offline)


class OfflineTargetTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "target"
        layout = self.root / "oci"
        self.blobs = layout / "blobs/sha256"
        self.blobs.mkdir(parents=True)
        (layout / "oci-layout").write_text('{"imageLayoutVersion":"1.0.0"}')

        def blob(data):
            raw = json.dumps(data, separators=(",", ":")).encode() if isinstance(data, dict) else data
            digest = hashlib.sha256(raw).hexdigest()
            (self.blobs / digest).write_bytes(raw)
            return {"digest": "sha256:" + digest, "size": len(raw)}

        self.layer = blob(b"actual-os-payload")
        config = blob({"architecture": "arm64", "os": "linux"})
        target = blob({"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json",
                       "config": config, "layers": [self.layer]})
        signature = blob({"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json",
                          "config": config, "layers": [blob(b"signature attachment")]})
        target["mediaType"] = signature["mediaType"] = "application/vnd.oci.image.manifest.v1+json"
        (layout / "index.json").write_text(json.dumps({
            "schemaVersion": 2, "manifests": [target, signature]}))
        self.ref = "code.podesta.ai/qos/tilo@" + target["digest"]
        (self.root / "target.json").write_text(json.dumps({
            "schema": "tilo.installer-offline-target.v1",
            "target_ref": self.ref,
            "target_digest": target["digest"],
            "architecture": "aarch64",
            "hardware_profile": "lenovo-83ed",
            "update_origin": "code.podesta.ai/qos/tilo:lenovo-83ed-" + "a" * 40,
        }))

    def test_valid_target_and_copy(self):
        self.assertEqual(offline.verify(self.root)["target_ref"], self.ref)
        destination = Path(self.temp.name) / "staged"
        offline.install(self.root, destination)
        self.assertEqual(offline.verify(destination)["target_ref"], self.ref)

    def test_tampered_blob_rejected(self):
        (self.blobs / self.layer["digest"].removeprefix("sha256:")).write_bytes(b"tampered-os-payload")
        with self.assertRaisesRegex(ValueError, "blob"):
            offline.verify(self.root)

    def test_extra_transport_file_rejected(self):
        (self.root / "not-reviewed").write_text("x")
        with self.assertRaisesRegex(ValueError, "unexpected"):
            offline.verify(self.root)


if __name__ == "__main__":
    unittest.main()
