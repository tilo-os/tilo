import hashlib
import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from contextlib import redirect_stderr
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import tilo_sh_preflight as preflight


def digest(content):
    return "sha256:" + hashlib.sha256(content).hexdigest()


def archive_at(path, corrupt=False):
    """Write a single-platform, zero-layer OCI image in the same archive layout as the image builds."""
    config = json.dumps({"architecture": "amd64", "os": "linux", "rootfs": {"type": "layers", "diff_ids": []}}).encode()
    def descriptor(kind, data):
        return {"mediaType": "application/vnd.oci.image." + kind + ".v1+json", "size": len(data), "digest": digest(data)}

    manifest = json.dumps({"schemaVersion": 2, "config": descriptor("config", config), "layers": []}).encode()
    index = json.dumps({"schemaVersion": 2, "manifests": [descriptor("manifest", manifest)]}).encode()
    with tarfile.open(path, "w") as archive:
        for name, data in (("oci-layout", b'{"imageLayoutVersion":"1.0.0"}'), ("index.json", index),
                           (digest(manifest).replace(":", "/"), manifest + (b" " if corrupt else b"")),
                           (digest(config).replace(":", "/"), config)):
            entry = tarfile.TarInfo(name if name in ("oci-layout", "index.json") else "blobs/" + name)
            entry.size = len(data)
            archive.addfile(entry, io.BytesIO(data))
    return "registry.tilo.so/tilo/lounge@" + digest(manifest), digest(config)


class ArchivePreflight(unittest.TestCase):
    def setUp(self):
        work = tempfile.TemporaryDirectory()
        self.addCleanup(work.cleanup)
        self.root = Path(work.name).resolve()
        self.archive = self.root / "image.oci.tar"
        self.pin, self.config = archive_at(self.archive)
        self.container = {"name": "room-service", "image": self.pin}
        self.deployment = {"metadata": {"name": "tilo-sh-relay"},
                           "spec": {"template": {"spec": {"containers": [self.container]}}}}
        self.docs = {"Deployment": [self.deployment]}

    def test_only_pinned_archive_is_used_by_local_id_for_settings_and_pod(self):
        with patch.object(preflight, "podman", return_value=self.config.removeprefix("sha256:")) as podman:
            local = preflight.load_archives([(self.pin, self.archive)], self.docs)
            podman.assert_called_once_with("pull", "-q", f"oci-archive:{self.archive}")
            pod = preflight.Pod(self.deployment, {}, None, self.root, "release", local)
            image_id = self.config.removeprefix("sha256:")
            self.assertEqual(pod.args(self.container, True)[-2:], ["--pull=never", image_id])
            unchanged = {**self.container, "image": self.pin.replace("lounge@", "api@")}
            self.assertEqual(pod.args(unchanged, True)[-1], unchanged["image"])
            self.assertNotIn("--pull=never", pod.args(unchanged, True))
            with patch.object(preflight, "run") as run:
                run.return_value.stdout = "TILO_ROOM"
                self.assertEqual(preflight.image_names(self.pin, local), {"TILO_ROOM"})
                self.assertEqual(run.call_args.args[-1], image_id)
                self.assertEqual(podman.call_count, 1)

    def test_rejects_unknown_or_duplicate_pins_and_mismatched_content(self):
        with patch.object(preflight, "podman", return_value=self.config) as podman, redirect_stderr(io.StringIO()):
            for pin in (self.pin.replace("lounge@", "unrelated@"), self.pin.replace("lounge@", "api@"),
                        self.pin.rsplit("@", 1)[0] + ":candidate"):
                with self.subTest(pin=pin), self.assertRaises(SystemExit):
                    preflight.load_archives([(pin, self.archive)], self.docs)
            other = self.pin.split("@")[0] + "@sha256:" + "f" * 64
            self.container["image"] = other
            with self.assertRaises(SystemExit):
                preflight.load_archives([(other, self.archive)], self.docs)
            self.container["image"] = self.pin
            archive_at(self.archive, corrupt=True)
            with self.assertRaises(SystemExit):
                preflight.load_archives([(self.pin, self.archive)], self.docs)
            podman.assert_not_called()
            archive_at(self.archive)
            with self.assertRaises(SystemExit):
                preflight.load_archives([(self.pin, self.archive)] * 2, self.docs)
            podman.return_value = "0" * 64
            with self.assertRaises(SystemExit):
                preflight.load_archives([(self.pin, self.archive)], self.docs)


if __name__ == "__main__":
    unittest.main()
