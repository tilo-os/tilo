#!/usr/bin/env python3
"""Profile provenance and real packaging/catalog entrypoints with tiny fixtures."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
from restart_fixture import restart_fixture

REPO = Path(__file__).resolve().parents[2]
PACKAGING = REPO / "apps/imager/packaging"
spec = importlib.util.spec_from_file_location("media_provenance", PACKAGING / "media-provenance.py")
provenance = importlib.util.module_from_spec(spec)
spec.loader.exec_module(provenance)
IMAGE_ID = "sha256:" + "a" * 64


class MediaProfilesTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="tilo media profile ")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.qcow = self.root / "installer.qcow2"
        self.qcow.write_bytes(b"tiny installer bytes; never a block device")
        self.definition = provenance.load_definition("lenovo-83ed")
        self.profile = {
            "schema_version": 1, "id": "lenovo-83ed", "architecture": "aarch64",
            "devicetree": "/usr/lib/modules/fixture/dtb/" + self.definition["devicetree"],
            "devicetree_sha256": "b" * 64,
            "target_update_ref": "code.tilo.so/tilo/tilo:board-fixture",
        }

    def create_sidecar(self, qcow=None, image_id=IMAGE_ID):
        snapshot = {"profile_json": json.dumps(self.profile) + "\n", "definition": self.definition}
        with mock.patch.object(provenance.subprocess, "check_output", side_effect=[
            image_id + " arm64\n", json.dumps(snapshot)
        ]) as podman:
            path = provenance.create("localhost/tilo-installer:fixture", qcow or self.qcow)
        return path, podman.call_args_list

    def test_final_image_metadata_binding(self):
        with self.subTest(phase="final_image_is_read_by_immutable_id_and_profile_payload_is_bound"):
            path, calls = self.create_sidecar()
            command = calls[1].args[0]
            self.assertIn(IMAGE_ID.removeprefix("sha256:"), command)
            self.assertNotIn("localhost/tilo-installer:fixture", command)
            self.assertIn("--read-only", command)
            self.assertIn("--network=none", command)
            record = json.loads(path.read_text())
            self.assertEqual(record["profile_sha256"], hashlib.sha256(record["profile_json"].encode()).hexdigest())
            checked = provenance.validate(self.qcow, "aarch64", "lenovo-83ed")
            self.assertEqual(checked["source_image_id"], IMAGE_ID)
            self.assertEqual(checked["target_update_ref"], self.profile["target_update_ref"])
        restart_fixture(self)
        with self.subTest(phase="native_podman_bare_sha256_image_id_is_normalized"):
            path, calls = self.create_sidecar(image_id=IMAGE_ID.removeprefix("sha256:"))
            self.assertEqual(json.loads(path.read_text())["source_image_id"], IMAGE_ID)
            self.assertIn(IMAGE_ID.removeprefix("sha256:"), calls[1].args[0])
        restart_fixture(self)
        with self.subTest(phase="dangling_generic_sidecar_cannot_downgrade_to_legacy_missing_metadata"):
            Path(str(self.qcow) + ".hardware.json").symlink_to(self.root / "missing.json")
            with self.assertRaisesRegex(ValueError, "symlink"):
                provenance.validate(self.qcow, "aarch64", "generic")

    def test_consumer_facts_are_preserved_and_bound_to_the_exact_image(self):
        home = {"schema": 1, "archive_protocol": 2, "seed_protocol": 1,
                **{key: "a" * 64 for key in ("firstboot_user_sha256", "seed_helper_sha256",
                                             "prepared_helper_sha256", "tar_sha256")}}
        snapshot = {"profile_json": None, "definition": None, "data_consumers": {"home_archive": home}}
        with mock.patch.object(provenance.subprocess, "check_output", side_effect=[
            IMAGE_ID + " amd64\n", json.dumps(snapshot)
        ]) as podman:
            sidecar = provenance.create("mutable-tag", self.qcow)
        self.assertIn("'--data-consumer-catalog-facts'", podman.call_args_list[1].args[0][-1])
        self.assertEqual(json.loads(sidecar.read_bytes())["data_consumers"], {"home_archive": home})
        result = subprocess.run([sys.executable, str(PACKAGING / "media-provenance.py"), "consumer-facts",
                                 str(self.qcow), "x86_64", "generic"],
                                capture_output=True, timeout=30, check=True)
        self.assertEqual(json.loads(result.stdout), {"home_archive": home})
        self.qcow.write_bytes(b"substituted image")
        with self.assertRaises(ValueError):
            provenance.validate(self.qcow, "x86_64", "generic")
        applications = {"schema": 1, "subset_protocol": 2,
                        **{key: "b" * 64 for key in ("prepared_helper_sha256", "qualification_script_sha256",
                                                     "native_qualification_sha256")}}
        for bad in ({"applications": applications}, {"home_archive": dict(home, schema=True)},
                    {"home_archive": home, "unknown": True},
                    {"home_archive": home, "applications": applications,
                     "original_source": {"original_source_protocol": 5}}):
            with self.assertRaises(ValueError):
                provenance.validate_consumers(bad)

    def test_board_profile_provenance(self):
        with self.subTest(phase="board_media_needs_provenance_and_matching_declared_architecture"):
            self.assertIsNone(provenance.validate(self.qcow, "aarch64", "generic"))
            for arch, name in (("aarch64", "lenovo-83ed"), ("x86_64", "lenovo-83ed"),
                               ("aarch64", "../board"), ("aarch64", "undefined-board")):
                with self.subTest(arch=arch, name=name), self.assertRaises(ValueError):
                    provenance.validate(self.qcow, arch, name)
        restart_fixture(self)
        with self.subTest(phase="changed_qcow_profile_hash_or_requested_profile_is_rejected"):
            path, _ = self.create_sidecar()
            original = self.qcow.read_bytes()
            self.qcow.write_bytes(original + b"changed")
            with self.assertRaisesRegex(ValueError, "qcow2 changed"):
                provenance.validate(self.qcow, "aarch64", "lenovo-83ed")
            self.qcow.write_bytes(original)
            with self.assertRaisesRegex(ValueError, "packaging profile"):
                provenance.validate(self.qcow, "aarch64", "generic")
            record = json.loads(path.read_text())
            record["profile_sha256"] = "0" * 64
            path.write_text(json.dumps(record))
            with self.assertRaisesRegex(ValueError, "invalid profile"):
                provenance.validate(self.qcow, "aarch64", "lenovo-83ed")
        restart_fixture(self)
        with self.subTest(phase="untrusted_update_origin_or_changed_definition_cannot_be_recorded"):
            self.profile["target_update_ref"] = "untrusted.example/tilo:latest"
            with self.assertRaises(ValueError):
                self.create_sidecar()
            self.profile["target_update_ref"] = "code.tilo.so/tilo/tilo:board-fixture"
            self.definition["firmware"] = {}
            with self.assertRaisesRegex(ValueError, "reviewed packaging definition"):
                self.create_sidecar()
        restart_fixture(self)
        with self.subTest(phase="real_packagers_preserve_generic_v1_and_publish_distinct_board_v2_metadata"):
            self.packaging_fixture()
            for arch in ("x86_64", "aarch64"):
                result = self.package(arch)
                self.assertEqual(result.returncode, 0, result.stderr)
            generic_out = self.root / "catalog-generic"
            result = self.catalog(generic_out)
            self.assertEqual(result.returncode, 0, result.stderr)
            generic = json.loads((generic_out / "tilo-installer-catalog-v1.json").read_text())
            self.assertEqual(generic["schema"], 1)
            self.assertEqual({release["hardware_profile"] for release in generic["releases"]}, {"generic"})
            board_qcow = self.root / "board.qcow2"
            board_qcow.write_bytes(b"different board installer bytes")
            self.create_sidecar(board_qcow)
            result = self.package("aarch64", "lenovo-83ed", board_qcow)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue((self.assets / "tilo-installer-linux-aarch64.img.zst").is_file())
            self.assertTrue((self.assets / "tilo-installer-linux-aarch64-lenovo-83ed.img.zst").is_file())
            board_out = self.root / "catalog-board"
            result = self.catalog(board_out)
            self.assertEqual(result.returncode, 0, result.stderr)
            catalog = json.loads((board_out / "tilo-installer-catalog-v2.json").read_text())
            self.assertEqual(catalog["schema"], 2)
            self.assertEqual(len(catalog["releases"]), 3)
            board = next(release for release in catalog["releases"] if release["hardware_profile"] == "lenovo-83ed")
            generic = next(release for release in catalog["releases"] if release["hardware_profile"] == "generic" and release["arch"] == "aarch64")
            self.assertNotEqual(board["image_sha256"], generic["image_sha256"])
            self.assertEqual(board["provenance"]["source_image_id"], IMAGE_ID)
            self.assertTrue(board["url"].endswith("aarch64-lenovo-83ed.img.zst"))
            shutil.copyfile(self.assets / "catalog-aarch64-lenovo-83ed.json", self.assets / "catalog-duplicate.json")
            result = self.catalog(self.root / "catalog-duplicate")
            self.assertNotEqual(result.returncode, 0, "duplicate hardware identity was accepted")
        restart_fixture(self)
        with self.subTest(phase="real_packager_rejects_board_without_provenance_before_creating_media"):
            self.packaging_fixture()
            result = self.package("aarch64", "lenovo-83ed")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("provenance sidecar", result.stderr)
            self.assertFalse(self.assets.exists())

    def packaging_fixture(self):
        checkout = self.root / "repo"
        for relative in (
            "apps/imager/packaging/package-install-medium.sh",
            "apps/imager/packaging/build-release-catalog.sh",
            "apps/imager/packaging/media-provenance.py",
            "os/dev/hardware_profile.py", "os/hardware/profiles/lenovo-83ed.json",
        ):
            target = checkout / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text((REPO / relative).read_text(), newline="\n")
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        # Stub conversion/compression only. Actual scripts, provenance checks,
        # jq, SHA-256, sizes, filenames and catalog assembly run unchanged.
        for name, source_arg in (("qemu-img", "-2"), ("zstd", "-3")):
            stub = bin_dir / name
            if name == "zstd":
                body = (
                    '#!/bin/bash\nset -eu\n'
                    'if [ "${1:-}" = "--decompress" ]; then\n'
                    '  cat -- "${@: -1}"\n'
                    'else\n'
                    '  cp -- "${@: -3:1}" "${@: -1}"\n'
                    'fi\n'
                )
            else:
                body = f'#!/bin/bash\nset -eu\ncp -- "${{@: {source_arg}:1}}" "${{@: -1}}"\n'
            stub.write_text(body)
            stub.chmod(0o755)
        self.env = {"PATH": f"{bin_dir}{os.pathsep}{os.defpath}", "HOME": str(self.root),
                    "GITHUB_REF_NAME": "imager-v1.2.3", "LANG": "C.UTF-8"}
        self.packaging = checkout / "apps/imager/packaging"
        self.assets = self.root / "assets"

    def package(self, arch, profile="generic", qcow=None):
        return subprocess.run(
            ["/bin/bash", str(self.packaging / "package-install-medium.sh"),
             str(qcow or self.qcow), arch, str(self.assets)],
            env=self.env | {"TILO_HARDWARE_PROFILE": profile}, text=True,
            capture_output=True, timeout=15, check=False,
        )

    def catalog(self, output):
        return subprocess.run(
            ["/bin/bash", str(self.packaging / "build-release-catalog.sh"), str(self.assets), str(output)],
            env=self.env, text=True, capture_output=True, timeout=15, check=False,
        )


if __name__ == "__main__":
    unittest.main()
