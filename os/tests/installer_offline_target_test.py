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


def restart_fixture(test):
    """Give the next subTest phase of a merged test the fresh fixture it had
    as its own test: tear down, run cleanups, reset instance state, set up."""
    test.tearDown()
    test.doCleanups()
    fresh = type(test)(test._testMethodName).__dict__
    keep = {key: test.__dict__[key] for key in ("_outcome", "_subtest") if key in test.__dict__}
    test.__dict__.clear()
    test.__dict__.update(fresh, **keep)
    test.setUp()


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
        target["annotations"] = {"kind": "dev.cosignproject.cosign/image"}
        signature["annotations"] = {"kind": "dev.cosignproject.cosign/sigs"}
        (layout / "index.json").write_text(json.dumps({
            "schemaVersion": 2, "manifests": [target, signature]}))
        self.ref = "code.tilo.so/tilo/tilo@" + target["digest"]
        (self.root / "target.json").write_text(json.dumps({
            "schema": "tilo.installer-offline-target.v1",
            "target_ref": self.ref,
            "target_digest": target["digest"],
            "architecture": "aarch64",
            "hardware_profile": "lenovo-83ed",
            "update_origin": "code.tilo.so/tilo/tilo:lenovo-83ed-" + "a" * 40,
        }))

    def test_accepts_valid_targets(self):
        with self.subTest(phase="valid_target_and_copy"):
            self.assertEqual(offline.verify(self.root)["target_ref"], self.ref)
            destination = Path(self.temp.name) / "staged"
            offline.install(self.root, destination)
            self.assertEqual(offline.verify(destination)["target_ref"], self.ref)
        restart_fixture(self)
        with self.subTest(phase="generic_signed_child_transport"):
            metadata_path = self.root / "target.json"
            metadata = json.loads(metadata_path.read_text())
            metadata["hardware_profile"] = "generic"
            metadata["target_ref"] = "registry.tilo.so/tilo/tilo@" + metadata["target_digest"]
            metadata["update_origin"] = "registry.tilo.so/tilo/tilo:gen7"
            metadata_path.write_text(json.dumps(metadata))
            self.assertEqual(offline.verify(self.root)["hardware_profile"], "generic")

    def test_rejects_tampered_or_incomplete_targets(self):
        with self.subTest(phase="board_target_is_never_the_public_repository"):
            # A board image carries private firmware and stays on the forge.
            metadata_path = self.root / "target.json"
            metadata = json.loads(metadata_path.read_text())
            metadata["target_ref"] = "registry.tilo.so/tilo/tilo@" + metadata["target_digest"]
            metadata_path.write_text(json.dumps(metadata))
            with self.assertRaises(ValueError):
                offline.verify(self.root)
        restart_fixture(self)
        with self.subTest(phase="tampered_blob_rejected"):
            (self.blobs / self.layer["digest"].removeprefix("sha256:")).write_bytes(b"tampered-os-payload")
            with self.assertRaisesRegex(ValueError, "blob"):
                offline.verify(self.root)
        restart_fixture(self)
        with self.subTest(phase="missing_image_kind_rejected"):
            index_path = self.root / "oci/index.json"
            index = json.loads(index_path.read_text())
            index["manifests"][0].pop("annotations")
            index_path.write_text(json.dumps(index))
            with self.assertRaisesRegex(ValueError, "pinned target"):
                offline.verify(self.root)
        restart_fixture(self)
        with self.subTest(phase="extra_transport_file_rejected"):
            (self.root / "not-reviewed").write_text("x")
            with self.assertRaisesRegex(ValueError, "unexpected"):
                offline.verify(self.root)
        restart_fixture(self)
        with self.subTest(phase="wrong_platform_config_rejected"):
            layout = self.root / "oci"
            index = json.loads((layout / "index.json").read_text())
            target = index["manifests"][0]
            manifest_path = self.blobs / target["digest"].removeprefix("sha256:")
            manifest = json.loads(manifest_path.read_text())
            config = b'{"architecture":"amd64","os":"linux"}'
            config_digest = hashlib.sha256(config).hexdigest()
            (self.blobs / config_digest).write_bytes(config)
            manifest["config"] = {"digest": "sha256:" + config_digest, "size": len(config)}
            rewritten = json.dumps(manifest, separators=(",", ":")).encode()
            rewritten_digest = hashlib.sha256(rewritten).hexdigest()
            (self.blobs / rewritten_digest).write_bytes(rewritten)
            manifest_path.unlink()
            target.update(digest="sha256:" + rewritten_digest, size=len(rewritten))
            (layout / "index.json").write_text(json.dumps(index))
            metadata_path = self.root / "target.json"
            metadata = json.loads(metadata_path.read_text())
            metadata["target_digest"] = target["digest"]
            metadata["target_ref"] = "code.tilo.so/tilo/tilo@" + target["digest"]
            metadata_path.write_text(json.dumps(metadata))
            with self.assertRaisesRegex(ValueError, "Linux ARM64"):
                offline.verify(self.root)


if __name__ == "__main__":
    unittest.main()
