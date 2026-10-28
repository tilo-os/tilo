"""Synthetic OCI transport checks for the CI-preloaded installer target."""

import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import shutil
from types import SimpleNamespace
from unittest.mock import patch
from restart_fixture import restart_fixture

MODULE = Path(__file__).resolve().parents[1] / "dev/installer-offline-target.py"
spec = importlib.util.spec_from_file_location("installer_offline_target", MODULE)
offline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(offline)


class OfflineTargetTest(unittest.TestCase):
    def test_development_export_preserves_manifest_and_checks_source(self):
        metadata = self.dev_transport()
        script = MODULE.with_name("prepare-dev-installer-target.py")
        module_spec = importlib.util.spec_from_file_location("prepare_dev_target", script)
        prepare = importlib.util.module_from_spec(module_spec)
        module_spec.loader.exec_module(prepare)
        args = SimpleNamespace(image=metadata["target_ref"],
                               source_revision=metadata["source_revision"],
                               ci_run_id=metadata["ci_run_id"],
                               hardware_profile=metadata["hardware_profile"],
                               update_origin=metadata["update_origin"],
                               rpms_ref=None,
                               output=Path(self.temp.name) / "exported")
        def copy_layout(argv, *, check, **kwargs):
            self.assertTrue(check)
            if argv[:2] == ["podman", "pull"]:
                self.assertEqual(argv, ["podman", "pull", args.image])
                return SimpleNamespace(stdout="")
            if argv[:2] == ["podman", "run"]:
                # A board target that already carries its sealed UKI.
                self.assertIn(args.image, argv)
                return SimpleNamespace(stdout="sealed\n")
            self.assertEqual(argv, ["skopeo", "copy", "--preserve-digests",
                                    "docker://" + args.image, "oci:" + str(args.output / "oci") + ":target"])
            shutil.copytree(self.root / "oci", args.output / "oci")
        with patch.object(prepare.subprocess, "run", side_effect=copy_layout):
            self.assertEqual(prepare.prepare(args), metadata)
            args.output = Path(self.temp.name) / "wrong-source"
            args.source_revision = "f" * 40
            with self.assertRaisesRegex(ValueError, "source or flavour"):
                prepare.prepare(args)
        args.image = "registry.tilo.so/tilo/tilo@" + metadata["target_digest"]
        with patch.object(prepare.subprocess, "run") as external:
            with self.assertRaisesRegex(ValueError, "development target"):
                prepare.prepare(args)
            external.assert_not_called()

    def test_split_kernel_export_carries_a_runner_built_uki(self):
        metadata = self.dev_transport()
        script = MODULE.with_name("prepare-dev-installer-target.py")
        module_spec = importlib.util.spec_from_file_location("prepare_dev_target", script)
        prepare = importlib.util.module_from_spec(module_spec)
        module_spec.loader.exec_module(prepare)
        rpms = "registry.tilo.so/tilo/rpms@sha256:" + "d" * 64
        args = SimpleNamespace(image=metadata["target_ref"],
                               source_revision=metadata["source_revision"],
                               ci_run_id=metadata["ci_run_id"],
                               hardware_profile=metadata["hardware_profile"],
                               update_origin=metadata["update_origin"],
                               rpms_ref=rpms, output=Path(self.temp.name) / "exported")
        calls = []
        def external(argv, *, check, **kwargs):
            argv = [str(item) for item in argv]
            calls.append(argv)
            if argv[0] == "skopeo":
                shutil.copytree(self.root / "oci", args.output / "oci")
            elif argv[:2] == ["podman", "run"] and "construct" not in argv:
                return SimpleNamespace(stdout="unsealed\n")
            elif argv[:2] == ["podman", "build"]:
                # The boot adapter's own tool stage, from a context holding
                # exactly the files that stage copies.
                self.assertEqual(argv[argv.index("--target") + 1], "uki-tools")
                self.assertIn("RPMS_REF=" + rpms, argv)
                self.assertIn("BASE_IMAGE=" + args.image, argv)
                context = Path(argv[-1])
                self.assertTrue((context / "os/dev/uki-tools.packages").is_file())
                self.assertTrue((context / "os/dev/uki-hardware-profile.py").is_file())
            elif "construct" in argv:
                self.assertIn("--network=none", argv)
                self.assertIn(f"type=image,source={args.image},destination=/target", argv)
                scratch = next(item.split(":")[0] for item in argv if item.endswith(":/out"))
                (Path(scratch) / "target-uki.efi").write_bytes(b"MZ runner-built UKI")
            return SimpleNamespace(stdout="")
        with patch.object(prepare.subprocess, "run", side_effect=external):
            self.assertEqual(prepare.prepare(args), metadata)
        self.assertEqual((args.output / "target-uki.efi").read_bytes(), b"MZ runner-built UKI")
        self.assertIn(["podman", "rmi", "-f", next(argv[argv.index("-t") + 1] for argv in calls
                                                  if argv[:2] == ["podman", "build"])], calls)
        destination = Path(self.temp.name) / "staged"
        offline.install(args.output, destination, development=True)
        self.assertEqual((destination / "target-uki.efi").read_bytes(), b"MZ runner-built UKI")
        # Signed release transports keep their exact two-entry shape.
        with self.assertRaisesRegex(ValueError, "unexpected root entries"):
            offline.verify(args.output)
        # A split kernel without the pinned snapshot cannot build its UKI.
        args.rpms_ref, args.output = None, Path(self.temp.name) / "no-tools"
        calls.clear()
        with patch.object(prepare.subprocess, "run", side_effect=external), \
                self.assertRaisesRegex(ValueError, "RPM snapshot"):
            prepare.prepare(args)
        self.assertFalse(any(argv[:2] == ["podman", "build"] for argv in calls))

    def dev_transport(self, *, flavour="dev", revision="a" * 40):
        config = {"architecture": "arm64", "os": "linux", "config": {"Labels": {
            "org.opencontainers.image.revision": revision, "sh.tilo.image.flavour": flavour,
        }}}
        def blob(data):
            raw = json.dumps(data, separators=(",", ":")).encode()
            digest = "sha256:" + hashlib.sha256(raw).hexdigest()
            (self.blobs / digest.removeprefix("sha256:")).write_bytes(raw)
            return {"digest": digest, "size": len(raw)}
        configuration = blob(config)
        manifest = blob({"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json",
                         "config": configuration, "layers": [self.layer]})
        for path in self.blobs.iterdir():
            if path.name not in {row["digest"].removeprefix("sha256:")
                                 for row in (configuration, manifest, self.layer)}:
                path.unlink()
        manifest["mediaType"] = "application/vnd.oci.image.manifest.v1+json"
        manifest["annotations"] = {"org.opencontainers.image.ref.name": "target"}
        (self.root / "oci/index.json").write_text(json.dumps({
            "schemaVersion": 2, "manifests": [manifest]}))
        metadata = {"schema": "tilo.installer-dev-target.v1",
                    "target_ref": "registry.tilo.so/tilo/tilo-vm@" + manifest["digest"],
                    "target_digest": manifest["digest"], "architecture": "aarch64",
                    "hardware_profile": "lenovo-83ed",
                    "update_origin": "registry.tilo.so/tilo/tilo-vm:fixture-lenovo-83ed-ci",
                    "source_revision": "a" * 40, "ci_run_id": "75340"}
        (self.root / "target.json").write_text(json.dumps(metadata))
        return metadata

    def test_explicit_development_transport(self):
        metadata = self.dev_transport()
        with self.assertRaises(ValueError):
            offline.verify(self.root)
        self.assertEqual(offline.verify(self.root, development=True), metadata)
        destination = Path(self.temp.name) / "dev-staged"
        offline.install(self.root, destination, development=True)
        self.assertEqual(offline.verify(destination, development=True), metadata)
        (self.blobs / self.layer["digest"].removeprefix("sha256:")).write_bytes(b"corrupted payload")
        with self.assertRaisesRegex(ValueError, "blob"):
            offline.verify(self.root, development=True)

    def test_development_requires_bound_source_and_flavour(self):
        self.dev_transport(flavour="production")
        with self.assertRaisesRegex(ValueError, "source or flavour"):
            offline.verify(self.root, development=True)
        self.dev_transport(revision="b" * 40)
        with self.assertRaisesRegex(ValueError, "source or flavour"):
            offline.verify(self.root, development=True)
        self.dev_transport()
        metadata = json.loads((self.root / "target.json").read_text())
        metadata["update_origin"] = "registry.tilo.so/tilo/tilo:latest"
        (self.root / "target.json").write_text(json.dumps(metadata))
        with self.assertRaisesRegex(ValueError, "pin a supported"):
            offline.verify(self.root, development=True)

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
