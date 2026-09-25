"""The tracked release trust roots are the KMS-backed production identities.

keys/efi-signing.crt is the Tilo release EFI certificate owners enroll in
firmware db, and keys/tpm2-pcr-signing.public.pem is the PCR prediction
authority. Both private keys live in AWS KMS; the protected
TILO_EFI_SIGNING_CERT_SHA256 variable pins the certificate independently.
"""

import base64
import hashlib
import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
KEYS = REPO / 'keys'

RSA_ENCRYPTION = bytes.fromhex('06092a864886f70d0101010500')
SHA256_WITH_RSA = bytes.fromhex('06092a864886f70d01010b0500')
CODE_SIGNING = bytes.fromhex('06082b06010505070303')
RSA_2048_SPKI_LENGTH = 294


def pem_der(path: Path, label: str) -> bytes:
    lines = path.read_text(encoding='ascii').strip().splitlines()
    assert lines[0] == f'-----BEGIN {label}-----', path
    assert lines[-1] == f'-----END {label}-----', path
    return base64.b64decode(''.join(lines[1:-1]), validate=True)


class ReleaseTrustRootsTest(unittest.TestCase):
    def test_efi_certificate_is_an_rsa2048_code_signing_certificate(self) -> None:
        der = pem_der(KEYS / 'efi-signing.crt', 'CERTIFICATE')
        self.assertIn(SHA256_WITH_RSA, der)
        self.assertIn(CODE_SIGNING, der)
        self.assertIn(RSA_ENCRYPTION, der)
        # A 2048-bit modulus is encoded as a 257-byte INTEGER.
        self.assertIn(bytes.fromhex('0282010100'), der)

    def test_documented_efi_fingerprint_matches_the_certificate(self) -> None:
        der = pem_der(KEYS / 'efi-signing.crt', 'CERTIFICATE')
        fingerprint = hashlib.sha256(der).hexdigest()
        readme = (KEYS / 'README.md').read_text(encoding='utf-8')
        documented = set(re.findall(r'DER SHA-256 `([0-9a-f]{64})`', readme))
        self.assertEqual(documented, {fingerprint})

    def test_pcr_authority_is_an_rsa2048_public_key(self) -> None:
        der = pem_der(KEYS / 'tpm2-pcr-signing.public.pem', 'PUBLIC KEY')
        self.assertEqual(len(der), RSA_2048_SPKI_LENGTH)
        self.assertEqual(der[4:4 + len(RSA_ENCRYPTION) + 2][2:], RSA_ENCRYPTION)

    def test_no_private_key_material_is_tracked(self) -> None:
        for path in KEYS.iterdir():
            if path.suffix in {'.pem', '.crt', '.pub'} and 'private' not in path.name:
                self.assertNotIn('PRIVATE KEY', path.read_text(encoding='ascii'), path)


if __name__ == '__main__':
    unittest.main()
