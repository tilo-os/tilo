import importlib.util
import contextlib
import io
import json
from pathlib import Path
import tempfile
import threading
import unittest
import urllib.error
import urllib.request


MODULE_PATH = Path(__file__).parents[1] / "tilo_oci_registry.py"
SPEC = importlib.util.spec_from_file_location("tilo_oci_registry", MODULE_PATH)
registry = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(registry)


class OciFixture:
    def __init__(self, root: Path):
        self.root = root
        (root / "blobs/sha256").mkdir(parents=True)
        (root / "oci-layout").write_text('{"imageLayoutVersion":"1.0.0"}\n', encoding="utf-8")
        self.layer = b"local layer bytes"
        self.layer_digest = self.blob(self.layer)
        config_digest = self.blob(b'{"architecture":"arm64","os":"linux"}')
        manifest = registry.json_bytes({
            "schemaVersion": 2,
            "mediaType": registry.OCI_MANIFEST,
            "config": {"mediaType": "application/vnd.oci.image.config.v1+json", "digest": config_digest, "size": 37},
            "layers": [{"mediaType": "application/vnd.oci.image.layer.v1.tar", "digest": self.layer_digest, "size": len(self.layer)}],
        })
        self.manifest = manifest
        self.manifest_digest = self.blob(manifest)
        (root / "index.json").write_text(json.dumps({
            "schemaVersion": 2,
            "mediaType": registry.OCI_INDEX,
            "manifests": [{
                "mediaType": registry.OCI_MANIFEST,
                "digest": self.manifest_digest,
                "size": len(manifest),
                "annotations": {"org.opencontainers.image.ref.name": "latest"},
            }],
        }), encoding="utf-8")

    def blob(self, value: bytes) -> str:
        digest = registry.sha256_bytes(value)
        (self.root / "blobs/sha256" / digest.split(":", 1)[1]).write_bytes(value)
        return digest


class RegistryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.fixture = OciFixture(self.root)
        self.layout = registry.Layout(self.root)
        self.server = registry.Registry(("127.0.0.1", 0), self.layout, "qos/tilo")
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.base = f"http://127.0.0.1:{self.server.server_port}"

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)
        self.temporary.cleanup()

    def request(self, path: str, *, method: str = "GET", headers=None):
        return urllib.request.urlopen(
            urllib.request.Request(self.base + path, method=method, headers=headers or {}), timeout=2
        )

    def test_serves_probe_tag_digest_head_and_tags(self):
        with self.request("/v2/") as response:
            self.assertEqual(response.status, 200)
            self.assertEqual(response.headers["Docker-Distribution-Api-Version"], "registry/2.0")
        for reference in ("latest", self.fixture.manifest_digest):
            with self.request(f"/v2/qos/tilo/manifests/{reference}") as response:
                self.assertEqual(response.read(), self.fixture.manifest)
                self.assertEqual(response.headers["Content-Type"], registry.OCI_MANIFEST)
                self.assertEqual(response.headers["Docker-Content-Digest"], self.fixture.manifest_digest)
            with self.request(f"/v2/qos/tilo/manifests/{reference}", method="HEAD") as response:
                self.assertEqual(int(response.headers["Content-Length"]), len(self.fixture.manifest))
                self.assertEqual(response.read(), b"")
        with self.request("/v2/qos/tilo/tags/list") as response:
            self.assertEqual(json.load(response), {"name": "qos/tilo", "tags": ["latest"]})

    def test_blob_streaming_and_ranges(self):
        path = f"/v2/qos/tilo/blobs/{self.fixture.layer_digest}"
        with self.request(path) as response:
            self.assertEqual(response.read(), self.fixture.layer)
            self.assertEqual(int(response.headers["Content-Length"]), len(self.fixture.layer))
        with self.request(path, headers={"Range": "bytes=6-10"}) as response:
            self.assertEqual(response.status, 206)
            self.assertEqual(response.read(), self.fixture.layer[6:11])
            self.assertEqual(response.headers["Content-Range"], f"bytes 6-10/{len(self.fixture.layer)}")

    def test_rejects_unsafe_digest_and_wrong_repository(self):
        with self.assertRaises(urllib.error.HTTPError) as unsafe:
            self.request("/v2/qos/tilo/blobs/sha256:../../index.json")
        self.assertEqual(unsafe.exception.code, 404)
        unsafe.exception.close()
        with self.assertRaises(urllib.error.HTTPError) as wrong_name:
            self.request(f"/v2/other/tilo/manifests/{self.fixture.manifest_digest}")
        self.assertEqual(wrong_name.exception.code, 404)
        wrong_name.exception.close()
        with self.assertRaises(ValueError):
            self.layout.blob_path("sha256:../index.json")

    def test_resolution_verifies_manifest_content(self):
        digest, media_type = self.layout.resolve("latest", "qos/tilo")
        self.assertEqual(digest, self.fixture.manifest_digest)
        self.assertEqual(media_type, registry.OCI_MANIFEST)
        self.layout.blob_path(digest).write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError, "does not match"):
            self.layout.resolve("latest", "qos/tilo")

    def test_repository_qualified_tag_is_normalized_safely(self):
        index_path = self.root / "index.json"
        index = json.loads(index_path.read_text(encoding="utf-8"))
        index["manifests"][0]["annotations"]["org.opencontainers.image.ref.name"] = "qos/tilo:pr192"
        index_path.write_text(json.dumps(index), encoding="utf-8")
        layout = registry.Layout(self.root)
        self.assertEqual(layout.tags("qos/tilo"), {"pr192": self.fixture.manifest_digest})
        self.assertEqual(layout.resolve("pr192", "qos/tilo")[0], self.fixture.manifest_digest)
        self.assertEqual(layout.tags("other/tilo"), {})
        with self.assertRaises(FileNotFoundError):
            layout.resolve("pr192", "other/tilo")

    def test_ambiguous_normalized_tags_are_rejected(self):
        index_path = self.root / "index.json"
        index = json.loads(index_path.read_text(encoding="utf-8"))
        index["manifests"][0]["annotations"]["org.opencontainers.image.ref.name"] = "qos/tilo:pr192"
        conflicting = dict(index["manifests"][0])
        conflicting["digest"] = self.fixture.layer_digest
        conflicting["annotations"] = {"org.opencontainers.image.ref.name": "pr192"}
        index["manifests"].append(conflicting)
        index_path.write_text(json.dumps(index), encoding="utf-8")
        layout = registry.Layout(self.root)
        with self.assertRaisesRegex(ValueError, "ambiguous OCI tag"):
            layout.resolve("pr192", "qos/tilo")

    def test_resolve_only_prints_digest_and_default_is_loopback(self):
        arguments = registry.parser().parse_args(["--layout", str(self.root), "--resolve-only"])
        self.assertEqual(arguments.host, "127.0.0.1")
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            self.assertEqual(
                registry.main(["--layout", str(self.root), "--reference", "latest", "--resolve-only"]),
                0,
            )
        self.assertEqual(output.getvalue().strip(), self.fixture.manifest_digest)


if __name__ == "__main__":
    unittest.main()
