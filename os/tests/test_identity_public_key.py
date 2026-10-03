"""The CI preflight reads publisher keys without exposing private identities."""

import importlib.util
import json
from pathlib import Path
import shutil
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "dev/identity-public-key.py"
SPEC = importlib.util.spec_from_file_location("identity_public_key", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class IdentityPublicKeyTests(unittest.TestCase):
    @unittest.skipUnless(shutil.which("openssl"), "OpenSSL is required for Ed25519 derivation")
    def test_rfc8032_ed25519_seed(self):
        identity = json.dumps({"secret_key_hex":
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"}).encode()
        self.assertEqual(MODULE.public_key(identity),
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")

    def test_bad_seed_is_rejected(self):
        with self.assertRaises(ValueError):
            MODULE.public_key(b'{"secret_key_hex":"not-a-seed"}')


if __name__ == "__main__":
    unittest.main()
