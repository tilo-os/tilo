"""Exercise snapshot manifest verification without pulling a multi-GB snapshot."""

import gzip
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tarfile
import unittest
from unittest.mock import patch


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
            self.commands.append(command)
            if command[:3] == ["docker", "buildx", "imagetools"]:
                return served[command[-1].rsplit("@", 1)[1]]
            self.assertTrue(command[1].endswith("derive-rpm-package-set.py"))
            return "example-package\n"

        def fetch(registry, repository, blob_digest, size):
            self.fetched.append((registry, repository, blob_digest, size))
            self.assertEqual((registry, repository), ("code.example", "tilo/rpms"))
            payload = data if blob is None else blob
            if len(payload) != size or digest(payload) != blob_digest:
                raise ValueError("metadata layer does not match its manifest entry")
            return payload

        with patch.object(VERIFY.platform, "machine", return_value=host), \
             patch.object(VERIFY.subprocess, "check_output", side_effect=read), \
             patch.object(VERIFY, "fetch_blob", side_effect=fetch):
            VERIFY.verify(ref)

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
            with patch.object(VERIFY.subprocess, "check_output",
                              side_effect=subprocess.CalledProcessError(1, ["docker"])), \
                 patch.object(VERIFY.platform, "machine", return_value="x86_64"), \
                 self.assertRaises(subprocess.CalledProcessError):
                VERIFY.verify("code.example/tilo/rpms@sha256:" + "a" * 64)

    def test_workflows_verify_before_building(self):
        for name in ("build-image.yml", "release-relay.yml"):
            workflow = (SCRIPT.parents[2] / ".forgejo/workflows" / name).read_text()
            with self.subTest(workflow=name):
                self.assertIn('run: python3 os/dev/verify-rpm-snapshot-buildkit.py "$RPMS_REF"\n', workflow)
                self.assertNotIn('verify-rpm-snapshot-package-set.sh docker "$RPMS_REF"', workflow)


if __name__ == "__main__":
    unittest.main()
