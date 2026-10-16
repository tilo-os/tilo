"""Name-availability checks must distinguish absent crates from failed observation."""

import errno
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import urllib.error


SCRIPT = Path(__file__).resolve().parents[1] / "sdk/publish_check.py"
SPEC = importlib.util.spec_from_file_location("sdk_publish_check_under_test", SCRIPT)
check = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(check)
REGISTRY_SPEC = importlib.util.spec_from_file_location(
    "sdk_local_registry_under_test", SCRIPT.with_name("local_registry.py")
)
registry = importlib.util.module_from_spec(REGISTRY_SPEC)
assert REGISTRY_SPEC.loader is not None
REGISTRY_SPEC.loader.exec_module(registry)


class TakenNamesTests(unittest.TestCase):
    def test_registered_crate_is_taken(self):
        response = io.BytesIO(json.dumps({"crate": {"name": "tilo-example"}}).encode())
        with patch.object(check.urllib.request, "urlopen", return_value=response):
            self.assertEqual(check.taken_names(["tilo-example"]), {"tilo-example"})

    def test_only_not_found_means_available(self):
        error = urllib.error.HTTPError("https://crates.io", 404, "Not Found", {}, None)
        with patch.object(check.urllib.request, "urlopen", side_effect=error):
            self.assertEqual(check.taken_names(["tilo-example"]), set())

    def test_invalid_success_response_cannot_mark_a_name_available(self):
        for document in ({}, {"errors": [{"detail": "unavailable"}]},
                         {"crate": None}, {"crate": {}}, {"crate": []},
                         {"crate": {"name": ""}}, []):
            with self.subTest(document=document):
                response = io.BytesIO(json.dumps(document).encode())
                with patch.object(check.urllib.request, "urlopen", return_value=response):
                    with self.assertRaisesRegex(ValueError, "tilo-example.*cannot determine"):
                        check.taken_names(["tilo-example"])

    def test_service_and_transport_failures_propagate(self):
        for error in (urllib.error.HTTPError("https://crates.io", 503, "Unavailable", {}, None),
                      urllib.error.URLError("connection refused")):
            with self.subTest(error=error):
                with patch.object(check.urllib.request, "urlopen", side_effect=error):
                    with self.assertRaises(type(error)) as raised:
                        check.taken_names(["tilo-example"])
                    self.assertIs(raised.exception, error)

    def test_invalid_json_propagates(self):
        with patch.object(check.urllib.request, "urlopen", return_value=io.BytesIO(b"not json")):
            with self.assertRaises(json.JSONDecodeError):
                check.taken_names(["tilo-example"])


class LocalRegistryChecksumTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.out = self.root / "out"
        self.out.mkdir()
        self.cache = self.root / "registry/cache/index.crates.io-test"
        self.cache.mkdir(parents=True)
        self.filename = "example-1.0.0.crate"
        self.data = b"valid crate bytes"
        self.checksum = hashlib.sha256(self.data).hexdigest()
        patcher = patch.object(registry, "CARGO_HOME", self.root)
        patcher.start()
        self.addCleanup(patcher.stop)

    def place(self):
        registry.place_crate("example", "1.0.0", self.checksum, self.out)

    def test_corrupt_cache_cannot_enter_the_registry(self):
        (self.cache / self.filename).write_bytes(b"corrupt")
        with patch.object(registry, "fetch") as fetch:
            with self.assertRaisesRegex(SystemExit, "checksum mismatch"):
                self.place()
            fetch.assert_not_called()
        self.assertFalse((self.out / self.filename).exists())

    def test_existing_output_is_checked_before_reuse(self):
        target = self.out / self.filename
        target.write_bytes(b"partial download")
        with self.assertRaisesRegex(SystemExit, "checksum mismatch"):
            self.place()
        target.write_bytes(self.data)
        with patch.object(registry, "fetch") as fetch:
            self.place()
            fetch.assert_not_called()

    def test_valid_cache_preserves_link_and_cross_device_copy(self):
        source = self.cache / self.filename
        source.write_bytes(self.data)
        target = self.out / self.filename
        self.place()
        self.assertTrue(source.samefile(target))
        target.unlink()
        with patch.object(registry.os, "link", side_effect=OSError(errno.EXDEV, "cross-device")):
            self.place()
        self.assertEqual(target.read_bytes(), self.data)
        self.assertFalse(source.samefile(target))

    def test_target_created_during_link_is_checked(self):
        (self.cache / self.filename).write_bytes(self.data)
        for data in (self.data, b"corrupt"):
            with self.subTest(data=data):
                target = self.out / self.filename
                target.unlink(missing_ok=True)

                def competing_link(source, destination):
                    destination.write_bytes(data)
                    raise FileExistsError(destination)

                with patch.object(registry.os, "link", side_effect=competing_link):
                    if data == self.data:
                        self.place()
                    else:
                        with self.assertRaisesRegex(SystemExit, "checksum mismatch"):
                            self.place()

    def test_download_checksum_is_still_required(self):
        with patch.object(registry, "fetch", return_value=b"corrupt"):
            with self.assertRaisesRegex(SystemExit, "checksum mismatch"):
                self.place()
        self.assertFalse((self.out / self.filename).exists())
        with patch.object(registry, "fetch", return_value=self.data):
            self.place()
        self.assertEqual((self.out / self.filename).read_bytes(), self.data)


if __name__ == "__main__":
    unittest.main()
