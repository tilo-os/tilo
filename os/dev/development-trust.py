#!/usr/bin/env python3
"""Validate public-only development trust inputs; never generate or accept private keys.

This prepares build inputs only. It does not change an image, a firmware trust
store, release defaults, or the signing authority of any artifact.
"""
import argparse
import hashlib
import json
import os
import re
import stat
import subprocess
import tempfile
from pathlib import Path


FILES = ('cosign.pub', 'tpm2-pcr-signing.public.pem',
         'efi-signing.crt', 'registry-owner.pub')
LIMIT = 65536
CRYPTO = Path('/usr/local/bin/tilo-crypto')


def reject_redirected_path(path):
    for component in (path, *path.parents):
        if component.is_symlink() or getattr(component, 'is_junction', lambda: False)():
            raise ValueError('public input path must not traverse a link or junction')


def read_public(path):
    path = Path(path)
    reject_redirected_path(path)
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_size > LIMIT:
            raise ValueError(f'expected a bounded regular public input: {path.name}')
        data = stream.read(LIMIT + 1)
        after = os.fstat(stream.fileno())
        if (len(data) != before.st_size or len(data) > LIMIT
                or (before.st_size, before.st_mtime_ns, before.st_ctime_ns)
                != (after.st_size, after.st_mtime_ns, after.st_ctime_ns)):
            raise ValueError('public input changed during snapshot')
        return data


def rust_crypto(*args):
    try:
        result = subprocess.run([CRYPTO, *map(str, args)],
                                capture_output=True, timeout=120)
    except subprocess.TimeoutExpired:
        raise ValueError('Rust-native public key validation timed out') from None
    if result.returncode:
        raise ValueError('Rust-native crypto rejected the public key or certificate')
    return result.stdout


def pem(data, label):
    pattern = (rb'\s*-----BEGIN ' + label.encode() + rb'-----\r?\n'
               rb'[A-Za-z0-9+/=\r\n]+-----END ' + label.encode() + rb'-----\s*')
    if not re.fullmatch(pattern, data):
        raise ValueError(f'expected exactly one public {label} PEM block')


def identities(inputs, paths):
    # Native parsers must inspect the bytes whose manifest hashes were checked,
    # not reopen mutable caller paths after the check.
    with tempfile.TemporaryDirectory(prefix='tilo-public-identities-') as temporary:
        snapshots = {name: Path(temporary) / name for name in FILES}
        for name, path in snapshots.items():
            path.write_bytes(inputs[name])
        return _identities(inputs, snapshots)


def _identities(inputs, paths):
    result = {}
    cosign = inputs['cosign.pub']
    pem(cosign, 'PUBLIC KEY')
    cosign_identity = rust_crypto('p256-public-key-sha256', paths['cosign.pub']).strip()
    if not re.fullmatch(rb'[0-9a-f]{64}', cosign_identity):
        raise ValueError('Rust-native crypto returned an invalid Cosign key identity')
    result['cosign.pub'] = cosign_identity.decode('ascii')
    pcr = inputs['tpm2-pcr-signing.public.pem']
    pem(pcr, 'PUBLIC KEY')
    info = rust_crypto('rsa-public-key-info', paths['tpm2-pcr-signing.public.pem'])
    parsed = re.fullmatch(rb'(\d+) ([0-9a-f]{64})\n?', info)
    if parsed is None or int(parsed[1]) < 2048:
        raise ValueError('PCR signing requires an RSA public key of at least 2048 bits')
    result['tpm2-pcr-signing.public.pem'] = parsed[2].decode('ascii')
    certificate = inputs['efi-signing.crt']
    pem(certificate, 'CERTIFICATE')
    certificate_identity = rust_crypto('certificate-sha256', paths['efi-signing.crt']).strip()
    if not re.fullmatch(rb'[0-9a-f]{64}', certificate_identity):
        raise ValueError('Rust-native crypto returned an invalid certificate identity')
    result['efi-signing.crt'] = certificate_identity.decode('ascii')
    owner = inputs['registry-owner.pub'].strip()
    if not re.fullmatch(rb'[0-9a-f]{64}', owner) or owner == b'0' * 64:
        raise ValueError('registry owner must be a nonzero lowercase Ed25519 public key')
    result['registry-owner.pub'] = owner.decode('ascii')
    return result


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate manifest property')
        result[key] = value
    return result


def validate(directory, expected_manifest_sha256=None):
    directory = Path(directory)
    reject_redirected_path(directory)
    if directory.is_symlink() or not directory.is_dir():
        raise ValueError('public bundle must be a regular directory')
    if {p.name for p in directory.iterdir()} != {*FILES, 'manifest.json'}:
        raise ValueError('unexpected or missing public bundle files')
    manifest_bytes = read_public(directory / 'manifest.json')
    manifest_digest = hashlib.sha256(manifest_bytes).hexdigest()
    if expected_manifest_sha256 is not None:
        if not re.fullmatch(r'[0-9a-f]{64}', expected_manifest_sha256) or manifest_digest != expected_manifest_sha256:
            raise ValueError('development trust manifest does not match the independently supplied digest')
    manifest = json.loads(manifest_bytes, object_pairs_hook=unique_object)
    if (set(manifest) != {'schema', 'purpose', 'sha256'}
            or type(manifest['schema']) is not int or manifest['schema'] != 1
            or manifest['purpose'] != 'tilo-development'
            or not isinstance(manifest['sha256'], dict) or set(manifest['sha256']) != set(FILES)):
        raise ValueError('invalid development trust manifest')
    inputs = {name: read_public(directory / name) for name in FILES}
    for name, data in inputs.items():
        if hashlib.sha256(data).hexdigest() != manifest['sha256'][name]:
            raise ValueError(f'public input digest mismatch: {name}')
    paths = {name: directory / name for name in FILES}
    return {'schema': 1, 'purpose': 'tilo-development', 'manifest_sha256': manifest_digest,
            'identities': identities(inputs, paths)}


def create(sources, output):
    inputs = {name: read_public(path) for name, path in zip(FILES, sources, strict=True)}
    paths = dict(zip(FILES, map(Path, sources), strict=True))
    identities(inputs, paths)
    output = Path(output)
    reject_redirected_path(output)
    # Exclusive creation: never replace an existing bundle or tracked key tree.
    output.mkdir(mode=0o700, parents=False, exist_ok=False)
    for name, data in inputs.items():
        with (output / name).open('xb') as stream:
            stream.write(data)
    manifest = {'schema': 1, 'purpose': 'tilo-development',
                'sha256': {name: hashlib.sha256(data).hexdigest() for name, data in inputs.items()}}
    with (output / 'manifest.json').open('x', encoding='utf-8') as stream:
        json.dump(manifest, stream, indent=2, sort_keys=True)
        stream.write('\n')
    return validate(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    check = commands.add_parser('validate')
    check.add_argument('directory', type=Path)
    check.add_argument('--expected-manifest-sha256')
    prepare = commands.add_parser('create')
    for option in ('cosign-public', 'pcr-public', 'efi-certificate', 'registry-owner'):
        prepare.add_argument('--' + option, required=True, type=Path)
    prepare.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    try:
        result = (validate(args.directory, args.expected_manifest_sha256) if args.command == 'validate' else
                  create([args.cosign_public, args.pcr_public, args.efi_certificate,
                          args.registry_owner], args.output))
        print(json.dumps(result, indent=2, sort_keys=True))
    except (OSError, ValueError, TypeError) as error:
        parser.exit(1, f'development trust: {error}\n')


if __name__ == '__main__':
    main()
