"""Exercise snapshot manifest verification without pulling a multi-GB snapshot."""

import gzip
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import unittest
from unittest.mock import patch
from urllib.error import URLError


SCRIPT = Path(__file__).resolve().parents[1] / "dev/verify-rpm-snapshot-buildkit.py"
SPEC = importlib.util.spec_from_file_location("snapshot_buildkit", SCRIPT)
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)
OCI_INDEX = "application/vnd.oci.image.index.v1+json"
OCI_MANIFEST = "application/vnd.oci.image.manifest.v1+json"
GZIP_LAYER = "application/vnd.oci.image.layer.v1.tar+gzip"


def digest(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def layer(files):
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w") as archive:
        for name, content in files.items():
            info = tarfile.TarInfo(name)
            if content is None:
                info.type = tarfile.DIRTYPE
                archive.addfile(info)
            elif isinstance(content, tuple):
                info.type = tarfile.SYMTYPE
                info.linkname = content[0]
                archive.addfile(info)
            else:
                data = content.encode()
                info.size = len(data)
                archive.addfile(info, io.BytesIO(data))
    return gzip.compress(buffer.getvalue())


class SnapshotManifestTests(unittest.TestCase):
    def check(self, files=None, host="x86_64", arch=None, ref=None, blob=None, layer_type=GZIP_LAYER,
              manifest_bytes=None):
        files = {"repo/tilo-direct-package-specs.txt": "example-package\n"} if files is None else files
        data = layer(files)
        manifest = json.dumps({"mediaType": OCI_MANIFEST, "layers": [
            {"mediaType": GZIP_LAYER, "digest": digest(b"packages"), "size": 8},
            {"mediaType": layer_type, "digest": digest(data), "size": len(data)},
        ]}).encode()
        platform_arch = arch or {"x86_64": "amd64", "aarch64": "arm64"}.get(host, "amd64")
        index = json.dumps({"mediaType": OCI_INDEX, "manifests": [
            {"mediaType": OCI_MANIFEST, "digest": digest(manifest),
             "platform": {"os": "linux", "architecture": platform_arch}},
        ]}).encode()
        ref = ref or "code.example/tilo/rpms@" + digest(index)
        served = {digest(index): index, digest(manifest): manifest_bytes or manifest}
        self.commands = []
        self.fetched = []

        def read(command, **kwargs):
            self.assertTrue(command[1].endswith("derive-rpm-package-set.py"))
            return "example-package\n"

        def manifest_of(registry, repository, manifest_digest):
            self.commands.append((registry, repository, manifest_digest))
            self.assertEqual((registry, repository), ("code.example", "tilo/rpms"))
            return served[manifest_digest]

        def fetch(registry, repository, blob_digest, size):
            self.fetched.append((registry, repository, blob_digest, size))
            self.assertEqual((registry, repository), ("code.example", "tilo/rpms"))
            payload = data if blob is None else blob
            if len(payload) != size or digest(payload) != blob_digest:
                raise ValueError("metadata layer does not match its manifest entry")
            return payload

        with patch.object(VERIFY.platform, "machine", return_value=host), \
             patch.object(VERIFY.subprocess, "check_output", side_effect=read), \
             patch.object(VERIFY, "fetch_manifest", side_effect=manifest_of), \
             patch.object(VERIFY, "fetch_blob", side_effect=fetch):
            VERIFY.verify(ref)

    def test_a_single_platform_snapshot_is_checked_by_its_configuration(self):
        data = layer({"repo/tilo-direct-package-specs.txt": "example-package\n"})
        for arch, accepted in (("amd64", True), ("arm64", False)):
            config = json.dumps({"architecture": arch, "os": "linux"}).encode()
            manifest = json.dumps({
                "mediaType": OCI_MANIFEST,
                "config": {"digest": digest(config), "size": len(config)},
                "layers": [{"mediaType": GZIP_LAYER, "digest": digest(data), "size": len(data)}],
            }).encode()
            blobs = {digest(config): config, digest(data): data}

            def fetch(registry, repository, blob_digest, size):
                payload = blobs[blob_digest]
                self.assertEqual(len(payload), size)
                return payload

            with self.subTest(arch=arch), \
                 patch.object(VERIFY.platform, "machine", return_value="x86_64"), \
                 patch.object(VERIFY.subprocess, "check_output", return_value="example-package\n"), \
                 patch.object(VERIFY, "fetch_manifest", return_value=manifest), \
                 patch.object(VERIFY, "fetch_blob", side_effect=fetch):
                if accepted:
                    VERIFY.verify("code.example/tilo/rpms@" + digest(manifest))
                else:
                    with self.assertRaisesRegex(ValueError, "not a native linux/amd64"):
                        VERIFY.verify("code.example/tilo/rpms@" + digest(manifest))

    def test_reads_only_the_metadata_layer(self):
        for host in ("x86_64", "aarch64"):
            with self.subTest(host=host):
                self.check(host=host)
                self.assertEqual(len(self.fetched), 1)
                self.assertNotEqual(self.fetched[0][2], digest(b"packages"))

    def test_rejections(self):
        with self.subTest(phase="unpinned_snapshot_is_rejected_before_registry_access"):
            with self.assertRaises(ValueError):
                self.check(ref="code.example/tilo/rpms:latest")
            self.assertEqual(self.commands, [])
        with self.subTest(phase="unknown_host_is_rejected"):
            with self.assertRaises(ValueError):
                self.check(host="riscv64")
            self.assertEqual(self.commands, [])
        with self.subTest(phase="foreign_architecture_is_rejected_before_layers"):
            with self.assertRaisesRegex(ValueError, "no single linux/amd64"):
                self.check(arch="arm64")
            self.assertEqual(self.fetched, [])
        with self.subTest(phase="manifest_digest_is_checked"):
            with self.assertRaisesRegex(ValueError, "not sha256:"):
                self.check(manifest_bytes=b'{"mediaType": "' + OCI_MANIFEST.encode() + b'", "layers": []}')
        with self.subTest(phase="unexpected_layer_type"):
            with self.assertRaisesRegex(ValueError, "metadata layer last"):
                self.check(layer_type="application/vnd.oci.image.layer.v1.tar+zstd")
        with self.subTest(phase="blob_digest_is_checked"):
            with self.assertRaisesRegex(ValueError, "does not match"):
                self.check(blob=b"tampered")
        with self.subTest(phase="manifest_must_be_a_regular_file"):
            for files in ({"repo/tilo-direct-package-specs.txt": None},
                          {"repo/tilo-direct-package-specs.txt": ("elsewhere",)},
                          {"repo/other.txt": "x\n"}):
                with self.subTest(files=files), self.assertRaisesRegex(ValueError, "no regular package manifest"):
                    self.check(files=files)
        with self.subTest(phase="stale_package_manifest_is_fatal"):
            with self.assertRaisesRegex(ValueError, "does not match the current Containerfiles"):
                self.check(files={"repo/tilo-direct-package-specs.txt": "old-package\n"})
        with self.subTest(phase="registry_failure_is_fatal"):
            with patch.object(VERIFY, "fetch_manifest", side_effect=URLError("unreachable")), \
                 patch.object(VERIFY.platform, "machine", return_value="x86_64"), \
                 self.assertRaises(OSError):
                VERIFY.verify("code.example/tilo/rpms@sha256:" + "a" * 64)

    def test_compressed_metadata_cannot_expand_without_bound(self):
        with patch.object(VERIFY, "MAX_METADATA_CONTENT", 16 * 1024, create=True):
            with self.assertRaisesRegex(ValueError, "expanded metadata"):
                self.check(files={"repo/padding": "x" * (32 * 1024),
                                  VERIFY.MANIFEST_PATH: "example-package\n"})

    def test_manifest_aliases_are_checked_for_duplicates(self):
        for alias in ("././" + VERIFY.MANIFEST_PATH, "repo//tilo-direct-package-specs.txt"):
            with self.subTest(alias=alias), self.assertRaisesRegex(ValueError, "no regular package manifest"):
                self.check(files={VERIFY.MANIFEST_PATH: "example-package\n",
                                  alias: "old-package\n"})

    def test_safe_manifest_alias_is_still_verified(self):
        self.check(files={"././" + VERIFY.MANIFEST_PATH: "example-package\n"})

    def test_parent_path_is_not_the_repository_manifest(self):
        with self.assertRaisesRegex(ValueError, "no regular package manifest"):
            self.check(files={"../" + VERIFY.MANIFEST_PATH: "example-package\n"})

    def test_the_relay_image_verifies_its_snapshot_before_building(self):
        build = (SCRIPT.parents[2] / "tools/ci/publish/relay-build.sh").read_text()
        verify = build.index('python3 os/dev/verify-rpm-snapshot-buildkit.py "$rpms_ref"\n')
        self.assertLess(verify, build.index("bash tools/ci/publish/oci-build.sh"))


if __name__ == "__main__":
    unittest.main()
