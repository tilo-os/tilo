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
    def test_platform_and_range_inputs_are_validated_exactly(self):
        for system, machine, want in (
            ("Darwin", "arm64", "macosarm64"),
            ("Darwin", "x86_64", "macosx64"),
            ("Linux", "aarch64", "linuxarm64"),
            ("Linux", "x86_64", "linux64"),
        ):
            with self.subTest(platform=(system, machine)):
                self.assertEqual(build_host.platform_key(system, machine), want)
        with self.subTest(platform="unsupported riscv64"), self.assertRaises(ValueError):
            build_host.platform_key("Linux", "riscv64")

        # A range response must match the requested bytes exactly.
        for label, header, received, want in (
            ("exact match", "bytes 0-9/20", 10, True),
            ("missing header", None, 10, False),
            ("wider range than requested", "bytes 0-19/20", 20, False),
            ("short body", "bytes 0-9/20", 9, False),
        ):
            with self.subTest(range=label):
                self.assertIs(build_host.valid_range(header, 0, 9, 20, received), want)

    def test_archive_verification_requires_matching_size_and_sha256(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "sdk"
            path.write_bytes(b"sdk-content")
            entry = {
                "size": 11,
                "sha1": hashlib.sha1(b"sdk-content").hexdigest(),
                "sha256": hashlib.sha256(b"sdk-content").hexdigest(),
            }
            with self.subTest(case="sha256 is mandatory"):
                with self.assertRaisesRegex(ValueError, "requires"):
                    build_host.verify_archive(path, {"size": 11})
            with self.subTest(case="sha256 must be well formed"):
                with self.assertRaisesRegex(ValueError, "invalid"):
                    build_host.verify_archive(path, {"size": 11, "sha256": "z" * 64})
            for label, content, want in (
                ("matching content", b"sdk-content", True),
                ("bad checksum", b"bad-content", False),
                ("bad size", b"short", False),
            ):
                with self.subTest(case=label):
                    path.write_bytes(content)
                    self.assertIs(build_host.verify_archive(path, entry), want)


if __name__ == "__main__":
    unittest.main()
