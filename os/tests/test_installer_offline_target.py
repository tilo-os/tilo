#!/usr/bin/env python3
"""Authentication checks for a separate offline installed image."""

import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "usr/libexec/tilo-installer-offline-target.py"
SPEC = importlib.util.spec_from_file_location("offline_target", SCRIPT)
target = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(target)
SOURCE = "a" * 40
UPDATE = "code.tilo.so/tilo/tilo:lenovo-83ed-" + SOURCE


class OfflineTargetTests(unittest.TestCase):
    def test_direct_import_preserves_digest_and_uses_immutable_id(self):
        with tempfile.TemporaryDirectory() as directory:
            root, metadata, _, _ = self.fixture(directory)
            receipt = Path(directory) / 'receipt.json'
            receipt.write_text(json.dumps(metadata))
            image_id = 'sha256:' + 'e' * 64
            calls = []
            def command(*argv, **kwargs):
                calls.append(argv)
                if argv[0] == 'skopeo':
                    self.assertEqual(argv[1:4], ('--insecure-policy', 'copy', '--preserve-digests'))
                    view = Path(argv[4].removeprefix('oci:').removesuffix(':target'))
                    self.assertTrue((view / 'blobs').is_symlink())
                    self.assertEqual(len(json.loads((view / 'index.json').read_text())['manifests']), 1)
                    self.assertEqual(argv[5], 'containers-storage:localhost/tilo-offline-target:'
                                     + metadata['target_digest'][7:])
                    return ''
                if '{{.Id}}' in argv:
                    return image_id
                if '{{.Digest}}' in argv:
                    self.assertEqual(argv[-1], image_id)
                    return metadata['target_digest']
                self.assertEqual(argv[0:2], ('podman', 'run'))
                self.assertIn(image_id, argv)
                self.assertIn('--network=none', argv)
                return ''
            with patch.object(target, 'RECEIPT', receipt), patch.object(target, 'run', command):
                self.assertEqual(target.import_image(root), image_id)
            self.assertFalse(any('load' in argv for argv in calls))
            with patch.object(target, 'RECEIPT', receipt), patch.object(target, 'run',
                    side_effect=['', image_id, 'sha256:' + 'f' * 64]):
                with self.assertRaisesRegex(ValueError, 'manifest differs'):
                    target.import_image(root)

    def test_command_failure_retains_exit_status_without_arguments(self):
        for code in (-9, 125):
            with self.subTest(returncode=code), patch.object(
                    target.subprocess, 'run', return_value=SimpleNamespace(
                        returncode=code, stdout='', stderr='')):
                with self.assertRaisesRegex(ValueError, f'exit {code}') as error:
                    target.run('podman', 'load', 'private-argument')
                self.assertIn('no diagnostic stderr', str(error.exception))
                self.assertNotIn('private-argument', str(error.exception))

    def test_development_transport_requires_the_vm_gate(self):
        with tempfile.TemporaryDirectory() as directory:
            root, metadata, _, _ = self.fixture(directory)
            blobs = root / "oci/blobs/sha256"
            def blob(data):
                raw = json.dumps(data, separators=(",", ":")).encode()
                digest = "sha256:" + hashlib.sha256(raw).hexdigest()
                (blobs / digest[7:]).write_bytes(raw)
                return {"digest": digest, "size": len(raw)}
            config = blob({"architecture": "arm64", "os": "linux", "config": {"Labels": {
                "sh.tilo.image.flavour": "dev", "org.opencontainers.image.revision": SOURCE,
            }}})
            layer = blob({"fixture": "payload"})
            manifest = blob({"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json",
                             "config": config, "layers": [layer]})
            for path in blobs.iterdir():
                if path.name not in {item["digest"][7:] for item in (config, layer, manifest)}:
                    path.unlink()
            manifest["mediaType"] = "application/vnd.oci.image.manifest.v1+json"
            manifest["annotations"] = {"org.opencontainers.image.ref.name": "target"}
            (root / "oci/index.json").write_text(json.dumps({"schemaVersion": 2, "manifests": [manifest]}))
            metadata.update(schema="tilo.installer-dev-target.v1", target_digest=manifest["digest"],
                            target_ref="registry.tilo.so/tilo/tilo-vm@" + manifest["digest"],
                            update_origin="registry.tilo.so/tilo/tilo-vm:fixture-lenovo-83ed-ci",
                            source_revision=SOURCE, ci_run_id="75356")
            (root / "target.json").write_text(json.dumps(metadata))
            with self.assertRaisesRegex(ValueError, "development VM installer"):
                target.read_target(root)
            marker = Path(directory) / "vm-marker"
            marker.write_text("VM-only")
            flavour = Path(directory) / "flavour"
            flavour.write_text("dev\n")
            with patch.object(target, "DEV_VM_MARKER", marker), patch.object(target, "DEV_FLAVOUR", flavour), \
                    patch.object(target, "VM_PROOF_PORT", Path("/dev/null")), \
                    patch.object(target, "run", return_value="") as command:
                selected, _ = target.read_target(root)
                self.assertEqual(selected, metadata)
                command.assert_called_once_with("systemd-detect-virt", "--vm", "--quiet")
                flavour.write_text("production\n")
                with self.assertRaisesRegex(ValueError, "development VM installer"):
                    target.read_target(root)

    def fixture(self, directory):
        root = Path(directory) / "target-oci"
        layout = root / "oci"
        blobs = layout / "blobs/sha256"
        blobs.mkdir(parents=True)
        config = b'{"architecture":"arm64","os":"linux","rootfs":{"type":"layers","diff_ids":[]}}'
        config_digest = hashlib.sha256(config).hexdigest()
        (blobs / config_digest).write_bytes(config)
        manifest = json.dumps({"schemaVersion": 2, "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": "sha256:" + config_digest, "size": len(config)},
            "layers": []}, separators=(",", ":")).encode()
        digest = "sha256:" + hashlib.sha256(manifest).hexdigest()
        (blobs / digest[7:]).write_bytes(manifest)
        (layout / "oci-layout").write_text('{"imageLayoutVersion":"1.0.0"}')
        (layout / "index.json").write_text(json.dumps({
            "schemaVersion": 2, "manifests": [{"mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": digest, "size": len(manifest),
                "annotations": {"kind": "dev.cosignproject.cosign/image"}}]}))
        metadata = {"schema": "tilo.installer-offline-target.v1",
                    "target_ref": "code.tilo.so/tilo/tilo@" + digest,
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
                             {"uri": "git+ssh://code.tilo.so/tilo/tilo@" + SOURCE},
                             {"uri": "code.tilo.so/tilo/tilo-lenovo-83ed-firmware@sha256:" + "c" * 64}]},
                         "runDetails": {"builder": {"id": target.BUILDER},
                                        "metadata": {"invocationId": "12345"},
                                        "byproducts": [{"name": "independent-rebuild",
                                                        "uri": "urn:tilo:verification:manifest-digest-equal"}]}}}
        signature = json.dumps([{"critical": {
            "identity": {"docker-reference": "code.tilo.so/tilo/tilo"},
            "image": {"docker-manifest-digest": digest}}}])
        attestation = json.dumps([{"payload": base64.b64encode(json.dumps(statement).encode()).decode()}])
        return root, metadata, signature, attestation

    def test_signed_target_binding(self):
        with self.subTest(phase="manifest_signature_and_slsa_bind_selected_target"):
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
        with self.subTest(phase="generic_child_binds_signed_generation_origin"):
            with tempfile.TemporaryDirectory() as directory:
                root, metadata, _, attestation = self.fixture(directory)
                metadata["hardware_profile"] = "generic"
                metadata["target_ref"] = "registry.tilo.so/tilo/tilo@" + metadata["target_digest"]
                metadata["update_origin"] = "registry.tilo.so/tilo/tilo:gen2"
                (root / "target.json").write_text(json.dumps(metadata))
                target.read_target(root)
                wrapper = json.loads(attestation)[0]
                statement = json.loads(base64.b64decode(wrapper["payload"]))
                external = statement["predicate"]["buildDefinition"]["externalParameters"]
                external.clear()
                external["generation"] = 2
                statement["predicate"]["buildDefinition"]["resolvedDependencies"].pop()
                wrapper["payload"] = base64.b64encode(json.dumps(statement).encode()).decode()
                signed = json.dumps([wrapper])
                provenance = target.checked_provenance(signed, metadata)
                self.assertEqual(provenance["generation"], 2)
                self.assertIsNone(provenance["firmware_candidate"])
                metadata["update_origin"] = "registry.tilo.so/tilo/tilo:gen3"
                with self.assertRaisesRegex(ValueError, "SLSA"):
                    target.checked_provenance(signed, metadata)

    def test_tamper_rejections(self):
        with self.subTest(phase="tampered_manifest_or_origin_fails"):
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
        with self.subTest(phase="ambiguous_or_unsigned_target_fails"):
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
        with self.subTest(phase="tampered_config_fails_before_disk_work"):
            with tempfile.TemporaryDirectory() as directory:
                root, metadata, _, _ = self.fixture(directory)
                manifest = json.loads((root / "oci/blobs/sha256" /
                                       metadata["target_digest"][7:]).read_text())
                config_path = root / "oci/blobs/sha256" / manifest["config"]["digest"][7:]
                config = json.loads(config_path.read_text())
                config["architecture"] = "amd64"
                config_path.write_text(json.dumps(config))
                with self.assertRaisesRegex(ValueError, "config differs"):
                    target.read_target(root)
        with self.subTest(phase="import_rejects_metadata_changed_after_preflight"):
            with tempfile.TemporaryDirectory() as directory:
                root, metadata, _, _ = self.fixture(directory)
                receipt = Path(directory) / "offline-target.json"
                receipt.write_text(json.dumps(metadata))
                metadata["update_origin"] = UPDATE[:-1] + "b"
                (root / "target.json").write_text(json.dumps(metadata))
                with patch.object(target, "RECEIPT", receipt), \
                        patch.object(target, "run") as run:
                    with self.assertRaisesRegex(ValueError, "changed after verified preflight"):
                        target.import_image(root)
                    run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
