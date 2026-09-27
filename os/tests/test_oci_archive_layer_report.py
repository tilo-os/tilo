#!/usr/bin/env python3
"""Exercise the small CI artifact that pinpoints independent image drift."""

import hashlib
import io
import json
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "scripts/oci-archive-layer-report.py"
SOURCE = "a" * 40
SECRET = "private-signing-key-must-not-enter-report"


def blob(data: dict) -> tuple[str, bytes]:
    encoded = json.dumps(data, separators=(",", ":")).encode()
    return "sha256:" + hashlib.sha256(encoded).hexdigest(), encoded


def archive(path: Path, *, source: str = SOURCE, corrupt_config: bool = False) -> None:
    config = {
        "architecture": "arm64", "os": "linux", "created": "2026-09-27T14:48:28Z",
        "config": {"Labels": {"org.opencontainers.image.revision": source},
                   "Env": ["SECRET=" + SECRET]},
        "rootfs": {"diff_ids": ["sha256:" + "1" * 64]},
        "history": [{"created_by": SECRET}],
    }
    config_digest, config_bytes = blob(config)
    manifest_digest, manifest_bytes = blob({
        "schemaVersion": 2,
        "config": {"digest": config_digest, "size": len(config_bytes)},
        "layers": [{"digest": "sha256:" + "2" * 64, "size": 123}],
    })
    index_bytes = json.dumps({"manifests": [{"digest": manifest_digest}]}).encode()
    with tarfile.open(path, "w") as output:
        for name, data in (
            ("index.json", index_bytes),
            ("blobs/sha256/" + manifest_digest[7:], manifest_bytes),
            ("blobs/sha256/" + config_digest[7:],
             config_bytes + (b"x" if corrupt_config else b"")),
        ):
            info = tarfile.TarInfo(name)
            info.size = len(data)
            output.addfile(info, io.BytesIO(data))


class ReportTests(unittest.TestCase):
    def test_report_is_bound_to_source_and_does_not_expose_secret_values(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "image.tar"
            output = Path(directory) / "report.json"
            archive(path)
            subprocess.run([sys.executable, str(SCRIPT), str(path), str(output),
                            "--source", SOURCE, "--run-id", "39251",
                            "--arch", "aarch64", "--role", "reproduce"], check=True)
            raw = output.read_text()
            self.assertNotIn(SECRET, raw)
            result = json.loads(raw)
            self.assertEqual((result["source"], result["run_id"], result["arch"],
                              result["role"]), (SOURCE, "39251", "aarch64", "reproduce"))
            self.assertEqual(len(result["layers"]), 1)
            self.assertEqual(result["layers"][0]["size"], 123)

    def test_wrong_source_or_tampered_config_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "image.tar"
            output = Path(directory) / "report.json"
            archive(path)
            command = [sys.executable, str(SCRIPT), str(path), str(output),
                       "--source", "b" * 40, "--run-id", "39251",
                       "--arch", "aarch64", "--role", "primary"]
            self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)
            self.assertFalse(output.exists())
            archive(path, corrupt_config=True)
            command[command.index("b" * 40)] = SOURCE
            self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
