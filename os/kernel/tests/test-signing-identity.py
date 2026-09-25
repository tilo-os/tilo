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


if __name__ == "__main__":
    unittest.main()
