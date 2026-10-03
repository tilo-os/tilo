"""oci-image-diff names the layers and files where two OCI images differ."""

import contextlib
import gzip
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "scripts/oci-image-diff.py"
SPEC = importlib.util.spec_from_file_location("oci_image_diff", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def layer(files):
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.PAX_FORMAT) as archive:
        for name, (content, mtime) in files.items():
            info = tarfile.TarInfo(name)
            info.size = len(content)
            info.mtime = mtime
            archive.addfile(info, io.BytesIO(content))
    tar = raw.getvalue()
    return gzip.compress(tar, mtime=0), "sha256:" + hashlib.sha256(tar).hexdigest()


def write_layout(root, layers, as_archive):
    blobs = {}

    def blob(data):
        digest = hashlib.sha256(data).hexdigest()
        blobs[digest] = data
        return f"sha256:{digest}"

    descriptors, diff_ids, history = [], [], []
    for files, created_by in layers:
        compressed, diff_id = layer(files)
        descriptors.append({"mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
                            "digest": blob(compressed), "size": len(compressed)})
        diff_ids.append(diff_id)
        history.append({"created_by": created_by})
    config = json.dumps({"architecture": "amd64", "os": "linux",
                         "rootfs": {"type": "layers", "diff_ids": diff_ids},
                         "history": history}).encode()
    manifest = json.dumps({"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json",
                           "config": {"mediaType": "application/vnd.oci.image.config.v1+json",
                                      "digest": blob(config), "size": len(config)},
                           "layers": descriptors}).encode()
    manifest_digest = blob(manifest)
    index = json.dumps({"schemaVersion": 2, "manifests": [
        {"mediaType": "application/vnd.oci.image.manifest.v1+json",
         "digest": manifest_digest, "size": len(manifest)}]}).encode()
    entries = {"oci-layout": b'{"imageLayoutVersion":"1.0.0"}', "index.json": index}
    entries.update({f"blobs/sha256/{digest}": data for digest, data in blobs.items()})
    if as_archive:
        path = root.with_suffix(".tar")
        with tarfile.open(path, "w") as archive:
            for name, data in entries.items():
                info = tarfile.TarInfo(name)
                info.size = len(data)
                archive.addfile(info, io.BytesIO(data))
        return str(path)
    for name, data in entries.items():
        (root / name).parent.mkdir(parents=True, exist_ok=True)
        (root / name).write_bytes(data)
    return str(root)


def run(*argv):
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        import sys
        saved = sys.argv
        sys.argv = ["oci-image-diff", *argv]
        try:
            status = MODULE.main()
        finally:
            sys.argv = saved
    return status, output.getvalue()


class OciImageDiffTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.root = Path(self.work.name)
        self.base = ({"usr/bin/tool": (b"\x7fELF same", 100)}, "RUN install tool")

    def tearDown(self):
        self.work.cleanup()

    def test_identical_images(self):
        a = write_layout(self.root / "a", [self.base], as_archive=True)
        b = write_layout(self.root / "b", [self.base], as_archive=False)
        status, output = run(a, b)
        self.assertEqual(status, 0)
        self.assertIn("manifests are identical", output)

    def test_names_differing_layer_files_and_text(self):
        a = write_layout(self.root / "a", [self.base, (
            {"etc/stamp": (b"one\nbuilt at 1\n", 100), "etc/same": (b"x", 100),
             "etc/touched": (b"y", 100)}, "RUN write stamp")], as_archive=True)
        b = write_layout(self.root / "b", [self.base, (
            {"etc/stamp": (b"one\nbuilt at 2\n", 100), "etc/same": (b"x", 100),
             "etc/touched": (b"y", 200), "etc/extra": (b"", 100)}, "RUN write stamp")], as_archive=False)
        status, output = run(a, b)
        self.assertEqual(status, 1)
        self.assertRegex(output, r"\n\s+0 same ")
        self.assertRegex(output, r"\n\s+1 DIFF .*RUN write stamp")
        self.assertIn("1 of 2 layers differ", output)
        self.assertIn("etc/stamp: content", output)
        self.assertIn("3 differing entries", output)
        self.assertIn("-built at 1", output)
        self.assertIn("+built at 2", output)
        self.assertIn("etc/touched: mtime 100 != 200", output)
        self.assertIn("only in B: etc/extra", output)
        self.assertNotIn("etc/same:", output)

    def test_reports_entry_order(self):
        files = {"a": (b"1", 1), "b": (b"2", 1)}
        a = write_layout(self.root / "a", [(files, "COPY")], as_archive=False)
        b = write_layout(self.root / "b", [(dict(reversed(list(files.items()))), "COPY")], as_archive=False)
        status, output = run(a, b)
        self.assertEqual(status, 1)
        self.assertIn("different entry order", output)


if __name__ == "__main__":
    unittest.main()
