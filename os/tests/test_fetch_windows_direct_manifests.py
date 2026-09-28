"""Contract tests for bounded, digest-pinned manifest fetches."""

import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SOURCE = Path(__file__).resolve().parents[1] / "dev" / "fetch-windows-direct-manifests.py"
SPEC = importlib.util.spec_from_file_location("fetch_windows_direct_manifests", SOURCE)
fetcher = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fetcher)


def encoded(value):
    raw = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    return raw, "sha256:" + hashlib.sha256(raw).hexdigest()


def image():
    return {"schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {"digest": "sha256:" + "a" * 64},
            "layers": [{"digest": "sha256:" + "b" * 64}]}


class FetchTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        base = Path(self.directory.name)
        self.paths = [base / name for name in ("index.json", "arm64.json", "lenovo.json")]
        self.child_raw, self.child_digest = encoded(image())
        self.lenovo_raw, self.lenovo_digest = encoded({**image(), "annotations": {"board": "83ed"}})
        self.index_raw, self.index_digest = encoded({
            "schemaVersion": 2, "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [
                {"mediaType": "application/vnd.oci.image.manifest.v1+json",
                 "digest": "sha256:" + "c" * 64,
                 "platform": {"os": "linux", "architecture": "amd64"}},
                {"mediaType": "application/vnd.oci.image.manifest.v1+json",
                 "digest": self.child_digest,
                 "platform": {"os": "linux", "architecture": "arm64", "variant": "v8"}}]})
        self.generic_ref = fetcher.PUBLIC + "@" + self.index_digest
        self.lenovo_ref = fetcher.BOARD + "@" + self.lenovo_digest
        self.objects = {self.generic_ref: self.index_raw,
                        fetcher.PUBLIC + "@" + self.child_digest: self.child_raw,
                        self.lenovo_ref: self.lenovo_raw}

    def run_fetch(self):
        return fetcher.fetch(self.generic_ref, self.lenovo_ref, *self.paths, "FedoraLinux-42")

    def test_fetches_only_pinned_manifests_and_reuses_verified_files(self):
        calls = []

        def fake(ref, distro):
            calls.append((ref, distro))
            return self.objects[ref]

        with patch.object(fetcher, "skopeo_raw", side_effect=fake):
            self.assertEqual(self.run_fetch(), self.child_digest)
        self.assertEqual([ref for ref, _ in calls], list(self.objects))
        self.assertEqual([path.read_bytes() for path in self.paths], list(self.objects.values()))
        with patch.object(fetcher, "skopeo_raw", side_effect=AssertionError("network called")):
            self.assertEqual(self.run_fetch(), self.child_digest)

    def test_changed_lenovo_bytes_publish_nothing(self):
        self.objects[self.lenovo_ref] += b" "
        with patch.object(fetcher, "skopeo_raw", side_effect=lambda ref, _: self.objects[ref]):
            with self.assertRaisesRegex(ValueError, "differ from the pinned digest"):
                self.run_fetch()
        self.assertTrue(all(not path.exists() for path in self.paths))

    def test_duplicate_arm_descriptor_is_rejected(self):
        index = json.loads(self.index_raw)
        index["manifests"][0]["platform"]["architecture"] = "arm64"
        index["manifests"][0]["platform"]["variant"] = "v8"
        self.index_raw, self.index_digest = encoded(index)
        self.generic_ref = fetcher.PUBLIC + "@" + self.index_digest
        self.objects[self.generic_ref] = self.index_raw
        with patch.object(fetcher, "skopeo_raw", side_effect=lambda ref, _: self.objects[ref]):
            with self.assertRaisesRegex(ValueError, "duplicate architecture"):
                self.run_fetch()
        self.assertTrue(all(not path.exists() for path in self.paths))

    def test_wrong_repository_and_mutable_tag_fail_without_fetch(self):
        with patch.object(fetcher, "skopeo_raw", side_effect=AssertionError("network called")):
            for bad in ("evil.test/tilo/tilo@" + self.index_digest,
                        fetcher.PUBLIC + ":latest"):
                with self.assertRaisesRegex(ValueError, "immutable sha256"):
                    fetcher.fetch(bad, self.lenovo_ref, *self.paths, "FedoraLinux-42")

    def test_skopeo_uses_raw_inspect_without_shell_or_auth_arguments(self):
        class Result:
            returncode = 0
            stdout = b"{}"

        with patch.object(fetcher.subprocess, "run", return_value=Result()) as run:
            self.assertEqual(fetcher.skopeo_raw(self.generic_ref, "FedoraLinux-42"), b"{}")
        args, kwargs = run.call_args
        self.assertEqual(args[0], ["wsl.exe", "-d", "FedoraLinux-42", "--exec",
                                   "skopeo", "inspect", "--raw", "docker://" + self.generic_ref])
        self.assertNotIn("shell", kwargs)
        self.assertIs(kwargs["stderr"], fetcher.subprocess.DEVNULL)


if __name__ == "__main__":
    unittest.main()
