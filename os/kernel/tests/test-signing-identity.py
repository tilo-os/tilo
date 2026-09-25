#!/usr/bin/env python3
import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "check-signing-identity.py"
spec = importlib.util.spec_from_file_location("check_signing_identity", SCRIPT)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class SigningIdentityTest(unittest.TestCase):
    def test_exact_signer_is_in_kernel(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image, built, signer = (root / name for name in ("vmlinux", "built.der", "signer.der"))
            image.write_bytes(b"kernel-prefix\x00certificate-one\x00kernel-suffix")
            built.write_bytes(b"certificate-one")
            signer.write_bytes(b"certificate-one")
            module.check(image, built, signer)

            signer.write_bytes(b"certificate-two")
            with self.assertRaisesRegex(ValueError, "differs"):
                module.check(image, built, signer)

            built.write_bytes(b"certificate-two")
            with self.assertRaisesRegex(ValueError, "absent"):
                module.check(image, built, signer)

            signer.write_bytes(b"")
            with self.assertRaisesRegex(ValueError, "differs"):
                module.check(image, built, signer)

    def test_staged_boot_image_must_contain_exact_signer(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image, boot, built, signer = (
                root / name for name in ("vmlinux", "vmlinuz", "built.der", "signer.der")
            )
            image.write_bytes(b"prefix-certificate-one-suffix")
            boot.write_bytes(b"prefix-certificate-two-suffix")
            built.write_bytes(b"certificate-one")
            signer.write_bytes(b"certificate-one")
            with self.assertRaisesRegex(ValueError, "staged boot image"):
                module.check(image, built, signer, boot)
            boot.write_bytes(image.read_bytes())
            module.check(image, built, signer, boot)

    def test_extracted_bzimage_must_contain_exact_signer(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image, boot, built, signer, extractor = (
                root / name for name in
                ("vmlinux", "bzImage", "built.der", "signer.der", "extract-vmlinux")
            )
            image.write_bytes(b"prefix-certificate-one-suffix")
            boot.write_bytes(b"prefix-certificate-two-suffix")
            built.write_bytes(b"certificate-one")
            signer.write_bytes(b"certificate-one")
            extractor.write_text('#!/bin/sh\ncat "$1"\n')
            with self.assertRaisesRegex(ValueError, "staged bzImage"):
                module.check(image, built, signer, boot, extractor)
            boot.write_bytes(image.read_bytes())
            module.check(image, built, signer, boot, extractor)
            extractor.write_text("#!/bin/sh\nexit 1\n")
            with self.assertRaisesRegex(ValueError, "could not extract"):
                module.check(image, built, signer, boot, extractor)

if __name__ == "__main__":
    unittest.main()
