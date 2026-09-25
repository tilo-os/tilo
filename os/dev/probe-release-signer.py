#!/usr/bin/python3
"""Prove a release signer before a publication spends hours building.

usage: probe-release-signer.py SIGNER EXPECTED_EFI_CERT_SHA256

The exported EFI certificate must match the independently pinned DER SHA-256
and keys/efi-signing.crt, and a real sign-sha256-rsa-pkcs1 signature must
verify with keys/tpm2-pcr-signing.public.pem. sign-target-prediction.sh still
repeats every check (and the root-owned signer path check) on the final image.
Standard library only: native runners carry no OpenSSL CLI by policy.
"""
import base64
import hashlib
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]


def pem_der(text: str, label: str) -> bytes:
    lines = text.strip().splitlines()
    if lines[0] != f'-----BEGIN {label}-----' or lines[-1] != f'-----END {label}-----':
        raise SystemExit(f'expected one PEM {label}')
    return base64.b64decode(''.join(lines[1:-1]), validate=True)


def tlv(data: bytes, offset: int) -> tuple[int, int, int]:
    tag, first = data[offset], data[offset + 1]
    if first < 0x80:
        return tag, offset + 2, offset + 2 + first
    count = first & 0x7F
    size = int.from_bytes(data[offset + 2:offset + 2 + count], 'big')
    return tag, offset + 2 + count, offset + 2 + count + size


def rsa_public(spki: bytes) -> tuple[int, int]:
    _, body, _ = tlv(spki, 0)
    _, _, algorithm_end = tlv(spki, body)
    tag, bits, bits_end = tlv(spki, algorithm_end)
    if tag != 0x03 or spki[bits] != 0:
        raise SystemExit('malformed RSA public key')
    key = spki[bits + 1:bits_end]
    _, body, _ = tlv(key, 0)
    _, n_start, n_end = tlv(key, body)
    _, e_start, e_end = tlv(key, n_end)
    return int.from_bytes(key[n_start:n_end], 'big'), int.from_bytes(key[e_start:e_end], 'big')


def verify_pkcs1_sha256(public: tuple[int, int], message: bytes, signature: bytes) -> bool:
    modulus, exponent = public
    size = (modulus.bit_length() + 7) // 8
    if len(signature) != size:
        return False
    info = bytes.fromhex('3031300d060960864801650304020105000420') + hashlib.sha256(message).digest()
    expected = b'\x00\x01' + b'\xff' * (size - len(info) - 3) + b'\x00' + info
    return pow(int.from_bytes(signature, 'big'), exponent, modulus).to_bytes(size, 'big') == expected


def main() -> None:
    signer, expected = sys.argv[1:]
    with tempfile.TemporaryDirectory(prefix='tilo-signer-probe.', dir='/var/tmp') as work:
        work = Path(work)
        subprocess.run([signer, 'export-efi-cert', str(work / 'efi-signing.crt')], check=True)
        exported = pem_der((work / 'efi-signing.crt').read_text(encoding='ascii'), 'CERTIFICATE')
        tracked = pem_der((REPO / 'keys/efi-signing.crt').read_text(encoding='ascii'), 'CERTIFICATE')
        actual = hashlib.sha256(exported).hexdigest()
        if actual != expected or exported != tracked:
            raise SystemExit('the release signer certificate matches neither '
                             'TILO_EFI_SIGNING_CERT_SHA256 nor keys/efi-signing.crt')
        probe = work / 'probe'
        probe.write_bytes(f'tilo release signer probe {expected}\n'.encode())
        subprocess.run([signer, 'sign-sha256-rsa-pkcs1', str(probe), str(work / 'probe.sig')],
                       check=True)
        public = rsa_public(pem_der(
            (REPO / 'keys/tpm2-pcr-signing.public.pem').read_text(encoding='ascii'), 'PUBLIC KEY'))
        if not verify_pkcs1_sha256(public, probe.read_bytes(), (work / 'probe.sig').read_bytes()):
            raise SystemExit('the release signer PCR signature does not verify with '
                             'keys/tpm2-pcr-signing.public.pem')
    print(f'release signer verified: EFI certificate {actual} and a KMS PCR signature')


if __name__ == '__main__':
    main()
