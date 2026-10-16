#!/usr/bin/python3
"""Verify a development boot loader before it enters the sealed root filesystem.

The certificate fingerprint must come from the independently pinned public trust
bundle. The JSON result records evidence; it is not an authorization token.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile

SPEC = importlib.util.spec_from_file_location(
    'authenticode', Path(__file__).with_name('verify-authenticode-envelope.py'))
envelope = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(envelope)
MACHINES = {'x86_64': 0x8664, 'aarch64': 0xAA64}


def snapshot(source, destination, limit):
    """Read through one held file descriptor, rejecting links and special files."""
    descriptor = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= limit:
            raise ValueError('loader inputs must be bounded, nonempty regular files')
        contents = stream.read(limit + 1)
        if len(contents) != info.st_size or len(contents) > limit:
            raise ValueError('loader input changed size while reading')
    destination.write_bytes(contents)
    return contents


def command(arguments):
    try:
        result = subprocess.run(arguments, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                timeout=120, env={'PATH': '/usr/bin:/bin', 'LANG': 'C'})
    except subprocess.TimeoutExpired:
        raise ValueError('trusted loader verification tool timed out') from None
    if result.returncode:
        raise ValueError(f'{Path(arguments[0]).name} rejected loader verification')
    return result.stdout


def verify(original, signed, certificate, certificate_sha256, architecture,
           maximum_input_bytes=8 * 1024 * 1024):
    if architecture not in MACHINES:
        raise ValueError('unsupported loader architecture')
    if not re.fullmatch('[0-9a-f]{64}', certificate_sha256):
        raise ValueError('expected a lowercase SHA256 certificate fingerprint')
    with tempfile.TemporaryDirectory(prefix='tilo-loader-check-') as temporary:
        root = Path(temporary)
        original_copy, signed_copy, certificate_copy = (
            root / 'original.efi', root / 'signed.efi', root / 'certificate.pem')
        if not isinstance(maximum_input_bytes, int) or not 0 < maximum_input_bytes <= 512 * 1024**2:
            raise ValueError('invalid EFI input bound')
        original_bytes = snapshot(original, original_copy, maximum_input_bytes)
        signed_bytes = snapshot(signed, signed_copy, maximum_input_bytes + 65536)
        snapshot(certificate, certificate_copy, 64 * 1024)
        # Parsing first bounds all subsequent PE fields, and verifies signing
        # preserved every byte outside the permitted Authenticode envelope.
        envelope.verify(str(original_copy), str(signed_copy))
        pe = envelope.u32(original_bytes, 0x3C)
        if envelope.u16(original_bytes, pe + 4) != MACHINES[architecture]:
            raise ValueError('loader PE machine does not match the native architecture')
        if envelope.u16(original_bytes, pe + 24) != 0x20B:
            raise ValueError('native loader must use PE32+')
        if envelope.u16(original_bytes, pe + 24 + 68) != 10:
            raise ValueError('loader must be an EFI application')
        actual_certificate = command(['/usr/local/bin/tilo-crypto', 'certificate-sha256',
                                      str(certificate_copy)]).decode().strip()
        if actual_certificate != certificate_sha256:
            raise ValueError('loader certificate does not match independently pinned identity')
        # A certificate-table envelope alone is insufficient: require a
        # recognized signature and verify it with the pinned certificate.
        count = command(['/usr/local/bin/tilo-crypto', 'efi-signature-count',
                         str(signed_copy)]).decode().strip()
        if not count.isdigit() or int(count) < 1:
            raise ValueError('loader has no recognized Authenticode signature')
        command(['/usr/local/bin/tilo-crypto', 'verify-efi',
                 str(certificate_copy), str(signed_copy)])
        return {'architecture': architecture, 'certificate_sha256': certificate_sha256,
                'original_sha256': hashlib.sha256(original_bytes).hexdigest(),
                'signed_sha256': hashlib.sha256(signed_bytes).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--original', type=Path, required=True)
    parser.add_argument('--signed', type=Path, required=True)
    parser.add_argument('--certificate', type=Path, required=True)
    parser.add_argument('--certificate-sha256', required=True)
    parser.add_argument('--architecture', choices=MACHINES, required=True)
    arguments = parser.parse_args()
    try:
        receipt = verify(**vars(arguments))
    except (OSError, ValueError) as error:
        parser.exit(1, f'development loader verification failed: {error}\n')
    print(json.dumps(receipt, sort_keys=True))


if __name__ == '__main__':
    main()
