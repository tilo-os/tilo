#!/usr/bin/env python3
"""OpenPGP v4 signatures and certificates made by an RSA key that never leaves AWS KMS.

rpmsign makes an RPM's OpenPGP signature by running `%__gpg_sign_cmd` on the bytes to sign.
Pointed at `sign`, this program hashes them here, asks KMS to sign only the SHA-256 digest
(`MessageType=DIGEST`, `RSASSA_PKCS1_V1_5_SHA_256`) and writes the OpenPGP v4 signature
packet (RFC 4880, 5.2.3) rpm reads back, issued by the key of the checked-in certificate.
Before writing it checks the signature against the certificate's public key, so a KMS key
that is not the certificate's fails here rather than in a verifier.

    openpgp_kms.py sign --certificate CERT --key KEY --output SIGNATURE [PLAINTEXT]
    openpgp_kms.py certificate --key KEY --created 2026-10-23T00:00:00Z --user-id UID OUTPUT
    openpgp_kms.py fingerprint CERT

KEY is a KMS key ARN (`arn:aws:kms:<region>:<account>:alias/<name>`), signed with the AWS
credentials in the environment (AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY, AWS_SESSION_TOKEN),
or `software:<file>`, a JSON RSA key {"n", "e", "d"} that stands in for KMS in tests.

`certificate` makes the certificate once: the public key KMS holds, with a fixed creation time,
one user ID and its self-signature, ASCII-armored. Making it again with the same inputs gives
the same key and fingerprint; only the self-signature's bytes differ, so it is made once and
checked in (docs/publishing-on-t.md, "RPM signing").

Python's standard library only: the builder image has no OpenSSL tools and no OpenPGP
implementation other than rpm's own.
"""
from __future__ import annotations

import argparse
import base64
import calendar
import datetime
import hashlib
import hmac
import json
import os
from pathlib import Path
import sys
import time
import urllib.error
import urllib.request

RSA = 1
SHA256 = 8
SIGNATURE_BINARY = 0x00
SIGNATURE_POSITIVE_CERTIFICATION = 0x13
# EMSA-PKCS1-v1_5's DigestInfo prefix for SHA-256 (RFC 8017, 9.2).
SHA256_DIGEST_INFO = bytes.fromhex('3031300d060960864801650304020105000420')
ARMOR_HEADER = '-----BEGIN PGP PUBLIC KEY BLOCK-----'
ARMOR_FOOTER = '-----END PGP PUBLIC KEY BLOCK-----'
KMS_ALGORITHM = 'RSASSA_PKCS1_V1_5_SHA_256'


class Refused(Exception):
    """Input this program will not sign or certify."""


# ---------------------------------------------------------------------------------------------
# OpenPGP encoding (RFC 4880)

def mpi(value: int) -> bytes:
    return value.bit_length().to_bytes(2, 'big') + value.to_bytes((value.bit_length() + 7) // 8, 'big')


def packet(tag: int, body: bytes) -> bytes:
    """A new-format packet (4.2.2)."""
    length = len(body)
    if length < 192:
        encoded = bytes([length])
    elif length < 8384:
        length -= 192
        encoded = bytes([(length >> 8) + 192, length & 0xFF])
    else:
        encoded = b'\xff' + length.to_bytes(4, 'big')
    return bytes([0xC0 | tag]) + encoded + body


def subpacket(kind: int, data: bytes) -> bytes:
    length = len(data) + 1
    if length < 192:
        encoded = bytes([length])
    elif length < 8384:
        length -= 192
        encoded = bytes([(length >> 8) + 192, length & 0xFF])
    else:
        encoded = b'\xff' + length.to_bytes(4, 'big')
    return encoded + bytes([kind]) + data


def public_key_body(n: int, e: int, created: int) -> bytes:
    return bytes([4]) + created.to_bytes(4, 'big') + bytes([RSA]) + mpi(n) + mpi(e)


def fingerprint_of(key_body: bytes) -> bytes:
    return hashlib.sha1(b'\x99' + len(key_body).to_bytes(2, 'big') + key_body).digest()


def emsa_pkcs1_v15(digest: bytes, size: int) -> bytes:
    encoded = SHA256_DIGEST_INFO + digest
    if size < len(encoded) + 11:
        raise Refused('the RSA key is too small for SHA-256')
    return b'\x00\x01' + b'\xff' * (size - len(encoded) - 3) + b'\x00' + encoded


def rsa_verifies(n: int, e: int, digest: bytes, signature: bytes) -> bool:
    size = (n.bit_length() + 7) // 8
    if len(signature) != size:
        return False
    value = int.from_bytes(signature, 'big')
    return value < n and pow(value, e, n).to_bytes(size, 'big') == emsa_pkcs1_v15(digest, size)


def armor(data: bytes) -> str:
    crc = 0xB704CE
    for byte in data:
        crc ^= byte << 16
        for _ in range(8):
            crc <<= 1
            if crc & 0x1000000:
                crc ^= 0x1864CFB
    encoded = base64.b64encode(data).decode('ascii')
    lines = [encoded[i:i + 64] for i in range(0, len(encoded), 64)]
    checksum = base64.b64encode((crc & 0xFFFFFF).to_bytes(3, 'big')).decode('ascii')
    return '\n'.join([ARMOR_HEADER, '', *lines, '=' + checksum, ARMOR_FOOTER, ''])


def dearmor(text: str) -> bytes:
    lines = [line.strip() for line in text.strip().splitlines()]
    if not lines or lines[0] != ARMOR_HEADER or lines[-1] != ARMOR_FOOTER or lines.count(ARMOR_HEADER) != 1:
        raise Refused('the certificate is not one ASCII-armored public key block')
    body = lines[1:-1]
    while body and ':' in body[0]:
        body.pop(0)
    if body and body[0] == '':
        body.pop(0)
    checksum = body.pop() if body and body[-1].startswith('=') else None
    data = base64.b64decode(''.join(body), validate=True)
    if checksum is not None and armor(data).splitlines()[-2] != checksum:
        raise Refused('the certificate armor checksum is wrong')
    return data


def packets(data: bytes):
    """(tag, body) for each packet; new and old formats, definite lengths only."""
    position = 0
    while position < len(data):
        first = data[position]
        if not first & 0x80:
            raise Refused('not an OpenPGP packet')
        if first & 0x40:
            tag = first & 0x3F
            octet = data[position + 1]
            if octet < 192:
                length, header = octet, 2
            elif octet < 224:
                length, header = ((octet - 192) << 8) + data[position + 2] + 192, 3
            elif octet == 255:
                length, header = int.from_bytes(data[position + 2:position + 6], 'big'), 6
            else:
                raise Refused('partial packet lengths are not supported')
        else:
            tag = (first >> 2) & 0x0F
            kind = first & 0x03
            if kind == 3:
                raise Refused('indeterminate packet lengths are not supported')
            size = 1 << kind
            length, header = int.from_bytes(data[position + 1:position + 1 + size], 'big'), 1 + size
        body = data[position + header:position + header + length]
        if len(body) != length:
            raise Refused('truncated OpenPGP packet')
        yield tag, body
        position += header + length


def read_mpi(data: bytes, position: int) -> tuple[int, int]:
    bits = int.from_bytes(data[position:position + 2], 'big')
    size = (bits + 7) // 8
    value = data[position + 2:position + 2 + size]
    if len(value) != size:
        raise Refused('truncated MPI')
    return int.from_bytes(value, 'big'), position + 2 + size


class Certificate:
    """The one RSA primary key of an armored public certificate."""

    def __init__(self, text: str):
        found = list(packets(dearmor(text)))
        if any(tag in (5, 7) for tag, _ in found):
            raise Refused('the certificate holds secret key material')
        primaries = [body for tag, body in found if tag == 6]
        if len(primaries) != 1:
            raise Refused('the certificate must hold exactly one primary key')
        body = primaries[0]
        if body[0] != 4 or body[5] != RSA:
            raise Refused('the certificate key is not a v4 RSA key')
        self.created = int.from_bytes(body[1:5], 'big')
        self.n, position = read_mpi(body, 6)
        self.e, position = read_mpi(body, position)
        if position != len(body):
            raise Refused('trailing bytes after the public key')
        self.key_body = body
        self.fingerprint = fingerprint_of(body)
        self.key_id = self.fingerprint[-8:]

    @classmethod
    def load(cls, path: Path) -> 'Certificate':
        return cls(path.read_text(encoding='ascii'))


def signature_packet(signer, sig_type: int, key_fingerprint: bytes, created: int,
                     hashed_extra: bytes, hashing, verify) -> bytes:
    """A v4 signature (5.2.3) whose HASHING (SHA-256) already holds what it covers: SIGNER signs
    the digest, and VERIFY must accept the signature before the packet is returned."""
    hashed = (subpacket(2, created.to_bytes(4, 'big')) + hashed_extra
              + subpacket(33, b'\x04' + key_fingerprint))
    unhashed = subpacket(16, key_fingerprint[-8:])
    head = bytes([4, sig_type, RSA, SHA256]) + len(hashed).to_bytes(2, 'big') + hashed
    hashing.update(head + b'\x04\xff' + len(head).to_bytes(4, 'big'))
    digest = hashing.digest()
    signature = signer.sign_digest(digest)
    if not verify(digest, signature):
        raise Refused("the signature does not verify with the certificate's key: "
                      "the signing key is another")
    body = (head + len(unhashed).to_bytes(2, 'big') + unhashed + digest[:2]
            + mpi(int.from_bytes(signature, 'big')))
    return packet(2, body)


def sign_document(signer, certificate: Certificate, source, created: int) -> bytes:
    """The binary-document signature packet (type 0x00) of the bytes read from SOURCE."""
    if created < certificate.created:
        raise Refused('the signature would predate its key')
    hashing = hashlib.sha256()
    while block := source.read(1024 * 1024):
        hashing.update(block)
    return signature_packet(signer, SIGNATURE_BINARY, certificate.fingerprint, created, b'', hashing,
                            lambda digest, signature: rsa_verifies(certificate.n, certificate.e,
                                                                   digest, signature))


def make_certificate(signer, created: int, user_id: str) -> str:
    """The armored certificate of SIGNER's public key: the key, USER_ID and its positive
    self-certification, which says the key certifies and signs and prefers SHA-256."""
    n, e = signer.public_key()
    key_body = public_key_body(n, e, created)
    key_fingerprint = fingerprint_of(key_body)
    uid = user_id.encode('utf-8')
    covered = (b'\x99' + len(key_body).to_bytes(2, 'big') + key_body
               + b'\xb4' + len(uid).to_bytes(4, 'big') + uid)
    # Key flags: certify and sign (0x03); preferred hashes SHA-256, SHA-512; features: MDC.
    extra = subpacket(27, b'\x03') + subpacket(21, bytes([SHA256, 10])) + subpacket(30, b'\x01')
    certification = signature_packet(
        signer, SIGNATURE_POSITIVE_CERTIFICATION, key_fingerprint, created, extra,
        hashlib.sha256(covered), lambda digest, signature: rsa_verifies(n, e, digest, signature))
    return armor(packet(6, key_body) + packet(13, uid) + certification)


def verify_self_certification(text: str) -> None:
    """Refuse a certificate whose user ID certification does not verify with its own key."""
    certificate = Certificate(text)
    found = list(packets(dearmor(text)))
    uids = [body for tag, body in found if tag == 13]
    signatures = [body for tag, body in found if tag == 2]
    if len(uids) != 1 or len(signatures) != 1:
        raise Refused('the certificate must hold one user ID and its one self-certification')
    uid, signature = uids[0], signatures[0]
    if signature[:4] != bytes([4, SIGNATURE_POSITIVE_CERTIFICATION, RSA, SHA256]):
        raise Refused('the self-certification is not a v4 RSA/SHA-256 positive certification')
    hashed_length = int.from_bytes(signature[4:6], 'big')
    head = signature[:6 + hashed_length]
    position = 6 + hashed_length
    unhashed_length = int.from_bytes(signature[position:position + 2], 'big')
    position += 2 + unhashed_length + 2
    value, end = read_mpi(signature, position)
    if end != len(signature):
        raise Refused('trailing bytes after the self-certification')
    covered = (b'\x99' + len(certificate.key_body).to_bytes(2, 'big') + certificate.key_body
               + b'\xb4' + len(uid).to_bytes(4, 'big') + uid)
    digest = hashlib.sha256(covered + head + b'\x04\xff' + len(head).to_bytes(4, 'big')).digest()
    size = (certificate.n.bit_length() + 7) // 8
    if not rsa_verifies(certificate.n, certificate.e, digest, value.to_bytes(size, 'big')):
        raise Refused('the self-certification does not verify')


# ---------------------------------------------------------------------------------------------
# Keys: AWS KMS, and a software key standing in for it in tests

def _der(data: bytes, position: int) -> tuple[int, bytes, int]:
    tag = data[position]
    length = data[position + 1]
    position += 2
    if length & 0x80:
        count = length & 0x7F
        length = int.from_bytes(data[position:position + count], 'big')
        position += count
    value = data[position:position + length]
    if len(value) != length:
        raise Refused('truncated DER')
    return tag, value, position + length


def rsa_public_key_from_spki(der: bytes) -> tuple[int, int]:
    """n and e of an RSA SubjectPublicKeyInfo, which KMS's GetPublicKey returns."""
    tag, spki, _ = _der(der, 0)
    if tag != 0x30:
        raise Refused('the public key is not a SubjectPublicKeyInfo')
    tag, algorithm, position = _der(spki, 0)
    _, oid, _ = _der(algorithm, 0)
    if tag != 0x30 or oid != bytes.fromhex('2a864886f70d010101'):
        raise Refused('the KMS key is not an RSA key')
    tag, bits, _ = _der(spki, position)
    if tag != 0x03 or bits[0] != 0:
        raise Refused('malformed public key bit string')
    _, sequence, _ = _der(bits, 1)
    tag_n, n, position = _der(sequence, 0)
    tag_e, e, _ = _der(sequence, position)
    if tag_n != 0x02 or tag_e != 0x02:
        raise Refused('malformed RSA public key')
    return int.from_bytes(n, 'big'), int.from_bytes(e, 'big')


class SoftwareKey:
    """A local RSA key, {"n": hex, "e": int, "d": hex}: what tests sign with instead of KMS."""

    def __init__(self, path: Path):
        key = json.loads(path.read_text())
        self.n, self.e, self.d = int(key['n'], 16), int(key['e']), int(key['d'], 16)

    def public_key(self) -> tuple[int, int]:
        return self.n, self.e

    def sign_digest(self, digest: bytes) -> bytes:
        size = (self.n.bit_length() + 7) // 8
        value = pow(int.from_bytes(emsa_pkcs1_v15(digest, size), 'big'), self.d, self.n)
        return value.to_bytes(size, 'big')


def generate_software_key(bits: int, e: int = 65537) -> dict:
    """A fresh RSA key in SoftwareKey's form, for tests: never a release key."""
    import secrets

    small = [p for p in range(3, 2000, 2) if all(p % q for q in range(3, int(p ** 0.5) + 1, 2))]

    def probable_prime(candidate: int) -> bool:
        if any(candidate % p == 0 for p in small):
            return False
        d, r = candidate - 1, 0
        while d % 2 == 0:
            d, r = d // 2, r + 1
        for _ in range(40):
            x = pow(secrets.randbelow(candidate - 3) + 2, d, candidate)
            if x in (1, candidate - 1):
                continue
            for _ in range(r - 1):
                x = pow(x, 2, candidate)
                if x == candidate - 1:
                    break
            else:
                return False
        return True

    def prime() -> int:
        while True:
            candidate = secrets.randbits(bits // 2) | (3 << (bits // 2 - 2)) | 1
            if probable_prime(candidate):
                return candidate

    while True:
        p, q = prime(), prime()
        phi = (p - 1) * (q - 1)
        if p != q and phi % e:
            return {'n': format(p * q, 'x'), 'e': e, 'd': format(pow(e, -1, phi), 'x')}


def sigv4_headers(region: str, target: str, body: bytes, credentials: dict, now: datetime.datetime) -> dict:
    """The headers of a KMS JSON request signed with AWS Signature Version 4."""
    host = f'kms.{region}.amazonaws.com'
    stamp = now.strftime('%Y%m%dT%H%M%SZ')
    day = stamp[:8]
    headers = {'content-type': 'application/x-amz-json-1.1', 'host': host,
               'x-amz-date': stamp, 'x-amz-target': target}
    if credentials.get('token'):
        headers['x-amz-security-token'] = credentials['token']
    names = sorted(headers)
    signed = ';'.join(names)
    canonical = '\n'.join(['POST', '/', '', ''.join(f'{name}:{headers[name]}\n' for name in names),
                           signed, hashlib.sha256(body).hexdigest()])
    scope = f'{day}/{region}/kms/aws4_request'
    to_sign = '\n'.join(['AWS4-HMAC-SHA256', stamp, scope, hashlib.sha256(canonical.encode()).hexdigest()])
    key = ('AWS4' + credentials['secret']).encode()
    for part in (day, region, 'kms', 'aws4_request'):
        key = hmac.new(key, part.encode(), hashlib.sha256).digest()
    signature = hmac.new(key, to_sign.encode(), hashlib.sha256).hexdigest()
    headers['authorization'] = (f'AWS4-HMAC-SHA256 Credential={credentials["access"]}/{scope}, '
                                f'SignedHeaders={signed}, Signature={signature}')
    return headers


class KmsKey:
    """An asymmetric KMS key, used with the session credentials in the environment."""

    def __init__(self, arn: str, environ=os.environ):
        parts = arn.split(':')
        if len(parts) < 6 or parts[:3] != ['arn', 'aws', 'kms'] or not parts[3]:
            raise Refused(f'not a KMS key ARN: {arn}')
        self.arn, self.region = arn, parts[3]
        self.credentials = {'access': environ.get('AWS_ACCESS_KEY_ID', ''),
                            'secret': environ.get('AWS_SECRET_ACCESS_KEY', ''),
                            'token': environ.get('AWS_SESSION_TOKEN', '')}
        if not self.credentials['access'] or not self.credentials['secret']:
            raise Refused('no AWS credentials in the environment (AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY)')

    def call(self, action: str, request: dict) -> dict:
        body = json.dumps(request).encode()
        headers = sigv4_headers(self.region, f'TrentService.{action}', body, self.credentials,
                                datetime.datetime.now(datetime.timezone.utc))
        del headers['host']
        call = urllib.request.Request(f'https://kms.{self.region}.amazonaws.com/', data=body,
                                      headers=headers, method='POST')
        try:
            with urllib.request.urlopen(call, timeout=60) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            answer = error.read().decode('utf-8', 'replace')[:500]
            raise Refused(f'KMS {action} answered HTTP {error.code}: {answer}') from None

    def public_key(self) -> tuple[int, int]:
        answer = self.call('GetPublicKey', {'KeyId': self.arn})
        if answer.get('KeyUsage') != 'SIGN_VERIFY' or KMS_ALGORITHM not in answer.get('SigningAlgorithms', []):
            raise Refused(f'{self.arn} is not a SIGN_VERIFY key that signs {KMS_ALGORITHM}')
        return rsa_public_key_from_spki(base64.b64decode(answer['PublicKey']))

    def sign_digest(self, digest: bytes) -> bytes:
        answer = self.call('Sign', {'KeyId': self.arn, 'Message': base64.b64encode(digest).decode(),
                                    'MessageType': 'DIGEST', 'SigningAlgorithm': KMS_ALGORITHM})
        if answer.get('SigningAlgorithm') != KMS_ALGORITHM:
            raise Refused('KMS signed with another algorithm')
        return base64.b64decode(answer['Signature'])


def key_from(spec: str):
    if spec.startswith('software:'):
        return SoftwareKey(Path(spec[len('software:'):]))
    return KmsKey(spec)


# ---------------------------------------------------------------------------------------------

def parse_time(value: str) -> int:
    return calendar.timegm(time.strptime(value, '%Y-%m-%dT%H:%M:%SZ'))


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split('\n\n', 1)[0])
    commands = parser.add_subparsers(dest='command', required=True)
    sign = commands.add_parser('sign', help='the detached signature rpmsign asks for')
    sign.add_argument('--certificate', type=Path, required=True)
    sign.add_argument('--key', required=True)
    sign.add_argument('--output', type=Path, required=True)
    sign.add_argument('plaintext', type=Path, nargs='?', help='what to sign (default: stdin)')
    certificate = commands.add_parser('certificate', help='make the certificate, once')
    certificate.add_argument('--key', required=True)
    certificate.add_argument('--created', required=True, help='UTC, YYYY-MM-DDTHH:MM:SSZ')
    certificate.add_argument('--user-id', required=True)
    certificate.add_argument('output', type=Path)
    fingerprint = commands.add_parser('fingerprint', help="a certificate's fingerprint")
    fingerprint.add_argument('certificate', type=Path)
    args = parser.parse_args(argv)
    try:
        if args.command == 'sign':
            cert = Certificate.load(args.certificate)
            key = key_from(args.key)
            created = int(time.time())
            if args.plaintext is None:
                signature = sign_document(key, cert, sys.stdin.buffer, created)
            else:
                with args.plaintext.open('rb') as source:
                    signature = sign_document(key, cert, source, created)
            args.output.write_bytes(signature)
        elif args.command == 'certificate':
            if args.output.exists():
                raise Refused(f'{args.output} exists: the certificate is made once')
            text = make_certificate(key_from(args.key), parse_time(args.created), args.user_id)
            verify_self_certification(text)
            args.output.write_text(text, encoding='ascii')
            print(Certificate(text).fingerprint.hex().upper())
        else:
            cert = Certificate.load(args.certificate)
            verify_self_certification(args.certificate.read_text(encoding='ascii'))
            print(cert.fingerprint.hex().upper())
    except (Refused, OSError, ValueError, KeyError) as error:
        print(f'openpgp_kms: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
