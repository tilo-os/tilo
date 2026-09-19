"""Runtime acquisition must fail closed on corrupt/unexpected SDK input."""
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("build_host", Path(__file__).with_name("build-host.py"))
build_host = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(build_host)


class AcquisitionTests(unittest.TestCase):
    def test_supported_platforms(self):
        self.assertEqual(build_host.platform_key("Darwin", "arm64"), "macosarm64")
        self.assertEqual(build_host.platform_key("Darwin", "x86_64"), "macosx64")
        self.assertEqual(build_host.platform_key("Linux", "aarch64"), "linuxarm64")
        self.assertEqual(build_host.platform_key("Linux", "x86_64"), "linux64")
        with self.assertRaises(ValueError):
            build_host.platform_key("Linux", "riscv64")

    def test_rejects_bad_size_and_checksum(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "sdk"
            path.write_bytes(b"sdk-content")
            entry = {
                "size": 11,
                "sha1": hashlib.sha1(b"sdk-content").hexdigest(),
                "sha256": hashlib.sha256(b"sdk-content").hexdigest(),
            }
            self.assertTrue(build_host.verify_archive(path, entry))
            path.write_bytes(b"bad-content")
            self.assertFalse(build_host.verify_archive(path, entry))
            path.write_bytes(b"short")
            self.assertFalse(build_host.verify_archive(path, entry))

    def test_sha256_is_mandatory_and_well_formed(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "sdk"
            path.write_bytes(b"sdk-content")
            with self.assertRaisesRegex(ValueError, "requires"):
                build_host.verify_archive(path, {"size": 11})
            with self.assertRaisesRegex(ValueError, "invalid"):
                build_host.verify_archive(path, {"size": 11, "sha256": "z" * 64})

    def test_range_response_must_match_requested_bytes(self):
        self.assertTrue(build_host.valid_range("bytes 0-9/20", 0, 9, 20, 10))
        self.assertFalse(build_host.valid_range(None, 0, 9, 20, 10))
        self.assertFalse(build_host.valid_range("bytes 0-19/20", 0, 9, 20, 20))
        self.assertFalse(build_host.valid_range("bytes 0-9/20", 0, 9, 20, 9))


if __name__ == "__main__":
    unittest.main()
