#!/usr/bin/env python3
"""Verify a hosted CLI's detached qualification and the four thin Mach-O signatures offline."""
import argparse
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import re
import struct
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location('hosted_cli_sign', ROOT / 'dist/apple/hosted-cli-sign.py')
native = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(native)

# Apple Inc Root, Developer ID G1 and G2, respectively. Public DER certificates are listed at
# https://www.apple.com/certificateauthority/ . The release worker never fetches trust material.
APPLE_ROOT = 'b0b1730ecbc7ff4505142c49f1295e6eda6bcaed7e2c68c5be91b5a11001f024'
DEVELOPER_ID_ISSUERS = {
    '7afc9d01a62f03a2de9637936d4afe68090d2de18d03f29c88cfb0b1ba63587f',
    'f16cd3c54c7f83cea4bf1a3e6a0819c8aaa8e4a1528fd144715f350643d2df3a',
}
# The fixed 168-byte prefix of `/usr/bin/csreq -r='designated => …' -b requirement` for
# native.product_requirement(team). Its only variable bytes are the 10-byte team and 2-byte pad.
REQUIREMENT_PREFIX_SHA256 = '2302b64e359c20389ff409a1697b42b9b01e7ba25bc1b84590b15a576dc481e8'
CERT = re.compile(rb'-----BEGIN CERTIFICATE-----\s*([A-Za-z0-9+/=\s]+?)\s*-----END CERTIFICATE-----')


def require(condition, message):
    if not condition:
        raise ValueError(message)


def openssl(*args, data=None):
    result = subprocess.run(['openssl', *map(str, args)], input=data, capture_output=True, timeout=40)
    if result.returncode:
        raise ValueError(f'OpenSSL {args[0]} failed: {result.stderr.decode(errors="replace").strip()}')
    return result.stdout


def fingerprint(pem):
    cert = CERT.fullmatch(pem.strip())
    require(cert, 'expected one PEM certificate')
    return hashlib.sha256(base64.b64decode(b''.join(cert[1].split()), validate=True)).hexdigest()


def verify_cms(signature, content, team, leaf_sha256, root, issuers):
    """Require detached signedData, one exact signer and the Developer ID CA chain."""
    require(0 < len(signature) <= 256 * 1024, 'empty or oversized CMS signature')
    with tempfile.TemporaryDirectory() as tmp:
        scratch = Path(tmp)
        blob, data = scratch / 'signature', scratch / 'content'
        leaf, certs, intermediate = scratch / 'leaf', scratch / 'chain', scratch / 'issuer'
        blob.write_bytes(signature)
        data.write_bytes(content)
        # -content takes precedence even for attached CMS. Check the actual outer content type
        # and detachment first. Nested timestamp tokens may carry their own attached content.
        description = openssl('cms', '-cmsout', '-print', '-inform', 'DER', '-in', blob)
        header = description.split(b'    certificates:', 1)[0]
        require(re.search(rb'^      eContent: <ABSENT>$', header, re.M), 'CMS must have detached content')
        digest_algorithms = header.partition(b'    digestAlgorithms:')[2].partition(b'    encapContentInfo:')[0]
        require(re.findall(rb'algorithm: ([\w-]+)', digest_algorithms) == [b'sha256'], 'CMS requires SHA-256')
        # The CMS signature is checked here. Its signer is bound to managed production pins and
        # the certificate path is verified below, avoiding OpenSSL's default S/MIME EKU policy.
        openssl('cms', '-verify', '-binary', '-noverify', '-inform', 'DER', '-in', blob,
                '-content', data, '-signer', leaf, '-certsout', certs, '-out', os.devnull)
        require(fingerprint(leaf.read_bytes()) == leaf_sha256, 'unexpected CMS signer certificate')
        subject = openssl('x509', '-in', leaf, '-noout', '-subject', '-nameopt', 'sep_multiline,sname,esc_ctrl,esc_msb')
        require(re.findall(rb'^    OU=(.*)$', subject, re.M) == [team.encode()], 'unexpected Apple team')
        require(re.search(rb'^    CN=Developer ID Application:', subject, re.M), 'not a Developer ID Application certificate')
        usage = openssl('x509', '-in', leaf, '-noout', '-ext', 'extendedKeyUsage')
        require(b'Code Signing' in usage, 'certificate lacks the code-signing extended key usage')
        extensions = openssl('x509', '-in', leaf, '-noout', '-text')
        require(re.search(rb'^\s*1\.2\.840\.113635\.100\.6\.1\.13:.*$', extensions, re.M),
                'missing Developer ID Application certificate extension')
        accepted = [m[0] for m in CERT.finditer(certs.read_bytes()) if fingerprint(m[0]) in issuers]
        require(len(accepted) == 1, 'missing or ambiguous Developer ID intermediate in CMS')
        intermediate.write_bytes(accepted[0] + b'\n')
        # OpenSSL does not interpret Apple's critical private OIDs. Here the exact public leaf
        # and CA are pinned, the Apple Application marker is required, and issuer/time/signatures
        # are independently checked. -trusted disables all ambient trust stores.
        openssl('verify', '-ignore_critical', '-trusted', root, intermediate)
        openssl('verify', '-ignore_critical', '-partial_chain', '-trusted', intermediate, leaf)


def slots(envelope):
    # Apple's CS_SuperBlob / CS_BlobIndex. Accept only our two bare programs' signature form.
    require(len(envelope) >= 12, 'missing code signature')
    magic, length, count = struct.unpack_from('>III', envelope)
    require(magic == 0xfade0cc0 and 3 <= count <= 5 and 12 + count * 8 <= length <= len(envelope),
            'unsupported code signature envelope')
    expected = {0: 0xfade0c02, 2: 0xfade0c01, 5: 0xfade7171, 7: 0xfade7172, 0x10000: 0xfade0b01}
    result, ranges = {}, []
    for i in range(count):
        kind, start = struct.unpack_from('>II', envelope, 12 + i * 8)
        require(kind in expected and kind not in result and 12 + count * 8 <= start <= length - 8,
                'unexpected or repeated code signature slot')
        slot_magic, size = struct.unpack_from('>II', envelope, start)
        require(slot_magic == expected[kind] and 8 <= size <= length - start, 'invalid code signature slot')
        end = start + size
        require(all(end <= a or start >= b for a, b in ranges), 'overlapping code signature slots')
        ranges.append((start, end))
        result[kind] = envelope[start:end]
    require({0, 2, 0x10000} <= result.keys(), 'missing CodeDirectory, requirement or CMS')
    return result


def check_directory(cd, image, special, team):
    # Only the SHA-256, non-scattered directory emitted by the hosted native signing recipe.
    # Apple's OS definitions are osfmk/kern/cs_blobs.h (CS_CodeDirectory). Unsupported hash
    # agility, pre-encryption and alternate CodeDirectories are rejected at this boundary.
    require(len(cd) >= 96, 'short CodeDirectory')
    header = struct.unpack_from('>9I4B', cd)
    magic, size, version, flags, offset, ident, n_special, n_pages, limit = header[:9]
    width, algorithm, platform, shift = header[9:]
    require(magic == 0xfade0c02 and size == len(cd) and version == 0x20500 and flags == 0x10000,
            'expected a hardened-runtime distribution CodeDirectory')
    require((width, algorithm, platform) == (32, 2, 0) and shift in (12, 14), 'unsupported CodeDirectory hashes')
    require(not any(cd[40:48] + cd[52:64] + cd[92:96]), 'unexpected CodeDirectory mapping')
    # CS_EXECSEG_MAIN_BINARY alone. Other bits grant JIT, debugging or unsigned-code exemptions.
    require(struct.unpack_from('>Q', cd, 80)[0] == 1, 'unexpected executable segment privileges')
    page = 1 << shift
    team_offset = struct.unpack_from('>I', cd, 48)[0]
    require(n_special == max(special) and limit == len(image) and n_pages == (limit + page - 1) // page and
            offset + 32 * n_pages == len(cd), 'CodeDirectory does not cover the whole executable')
    names_end = offset - 32 * n_special
    names = cd[:names_end]
    require(96 <= ident < names_end and names[ident:].startswith(native.IDENTIFIER.encode() + b'\0') and
            96 <= team_offset < names_end and names[team_offset:].startswith(team.encode() + b'\0'),
            'unexpected CodeDirectory product or team identifier')
    requirement = special[2]
    require(len(requirement) == 180 and hashlib.sha256(requirement[:-12]).hexdigest() == REQUIREMENT_PREFIX_SHA256 and
            requirement[-12:] == team.encode() + b'\0\0', 'unexpected designated requirement')
    if 5 in special:
        require(plistlib.loads(special[5][8:]) == {}, 'unexpected XML entitlements')
    if 7 in special:
        # derq's DER representation of the empty dictionary signed by the native recipe.
        require(special[7][8:] == bytes.fromhex('7005020101b000'), 'unexpected DER entitlements')
    for i in range(1, n_special + 1):
        wanted = hashlib.sha256(special[i]).digest() if i in special else bytes(32)
        end = offset - (i - 1) * 32
        require(cd[end - 32:end] == wanted, 'CodeDirectory special slot differs')
    view = memoryview(image)
    for i in range(n_pages):
        start = offset + i * width
        chunk = view[i * page:(i + 1) * page]
        require(cd[start:start + width] == hashlib.sha256(chunk).digest(), 'CodeDirectory page hash differs from executable')


def verify(raw, stage):
    team = os.environ.get('TILO_HOSTED_APPLE_TEAM_ID', '')
    leaf = os.environ.get('TILO_HOSTED_APPLE_LEAF_SHA256', '').lower()
    require(re.fullmatch('[A-Z0-9]{10}', team) and re.fullmatch('[a-f0-9]{64}', leaf),
            'configure the trusted release team and leaf fingerprint')
    root = ROOT / 'keys/release/apple-root-ca.pem'
    require(fingerprint(root.read_bytes()) == APPLE_ROOT, 'unexpected Apple Root CA')
    verify_stage(raw, stage, team, leaf, root, DEVELOPER_ID_ISSUERS)


def verify_stage(raw, stage, team, leaf, root, issuers):
    receipt = (stage / 'qualification.json').read_bytes()
    verify_cms((stage / 'qualification.cms').read_bytes(), receipt, team, leaf, root, issuers)
    qualification = json.loads(receipt)
    for arch, cpu in native.ARCHITECTURES.items():
        filename = f'tilo-macos-{arch}.tar.gz'
        with tempfile.TemporaryDirectory() as tmp:
            before, after = Path(tmp) / 'original', Path(tmp) / 'qualified'
            native.unpack(raw / filename, before, qualification['raw_sha256'][filename])
            native.unpack(stage / filename, after, qualification['signed_sha256'][filename])
            for name in native.PROGRAMS:
                image = (after / name).read_bytes()
                comparison = native.compare_signed_macho((before / name).read_bytes(), image, architecture=cpu)
                require(struct.unpack_from('<I', image, 12)[0] == 2, 'expected executable Mach-O')
                start = comparison['signature_offset']
                fields = slots(image[start:])
                cd, cms = fields.pop(0), fields.pop(0x10000)[8:]
                check_directory(cd, memoryview(image)[:start], fields, team)
                verify_cms(cms, cd, team, leaf, root, issuers)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['verify'])
    parser.add_argument('raw', type=Path)
    parser.add_argument('stage', type=Path)
    args = parser.parse_args()
    try:
        verify(args.raw, args.stage)
    except (KeyError, OSError, ValueError, struct.error) as error:
        parser.exit(1, f'hosted Apple release: {error}\n')
