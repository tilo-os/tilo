#!/usr/bin/env python3
"""Verify an exact signed capture candidate before embedding its public pins.

Artifact admission is not migration qualification. No result here grants access
or says that first use, source transfer or installed consumers have been tested.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import stat
import subprocess
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[3]
FIELDS = {'schema', 'worker_protocol', 'arch', 'package_id', 'service_publisher',
          'package_hash', 'manifest_sha256', 'payload_root_hash', 'runtime_version',
          'runtime_publisher', 'runtime_root_hash'}
DIGEST_FIELDS = FIELDS - {'schema', 'worker_protocol', 'arch', 'package_id', 'runtime_version'}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def unique(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, 'duplicate capture field')
        result[key] = value
    return result


def regular(path, limit):
    # Reject redirected ancestors too: the admitted location must be the one
    # watched by Cargo and subsequently copied into the package.
    for parent in (path, *path.parents):
        require(not parent.is_symlink(), 'redirected capture path')
    metadata = path.stat()
    require(stat.S_ISREG(metadata.st_mode) and 0 < metadata.st_size <= limit,
            'capture artifact is missing or outside bounds')
    return path.read_bytes()


def decode_pins(data, arch):
    require(0 < len(data) <= 16384, 'capture pins exceed bounds')
    pins = json.loads(data, object_pairs_hook=unique)
    require(isinstance(pins, dict) and set(pins) == FIELDS, 'unexpected capture pin fields')
    require(type(pins['schema']) is int and pins['schema'] == 1
            and type(pins['worker_protocol']) is int and pins['worker_protocol'] == 6,
            'full migration requires capture protocol 6')
    require(pins['arch'] == arch and arch in ('x86_64', 'aarch64'), 'capture architecture differs')
    require(pins['package_id'] == 'so.tilo.imager.home-export', 'capture package differs')
    for key in DIGEST_FIELDS:
        require(isinstance(pins[key], str) and re.fullmatch('[0-9a-f]{64}', pins[key]),
                'invalid capture digest')
    require(isinstance(pins['runtime_version'], str)
            and re.fullmatch(r'[0-9A-Za-z.-]+\+native\.[0-9a-f]{32}', pins['runtime_version']),
            'capture runtime is not an exact native build')
    return pins


def invoke(command):
    result = subprocess.run(command, check=True, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=120)
    require(len(result.stdout) <= 1024 * 1024 and len(result.stderr) <= 1024 * 1024,
            'capture verifier output exceeds bounds')


def admit(pins_path, sidecar, arch):
    pins_path = pins_path.absolute()
    sidecar = sidecar.absolute()
    original = regular(pins_path, 16384)
    pins = decode_pins(original, arch)
    packer = ROOT / 'target/release/tilo-app-package'
    verifier = ROOT / 'target/release/tilo-runtime-bundle'
    for tool in (packer, verifier):
        regular(tool, 512 * 1024 * 1024)
        require(tool.stat().st_mode & 0o111, 'capture verifier is not executable')
    require({entry.name for entry in sidecar.iterdir()} == {'service', 'runtime'},
            'unexpected capture sidecar contents')
    require({entry.name for entry in (sidecar / 'service').iterdir()} ==
            {'home-export.tapp', 'metadata', 'payload'}, 'unexpected capture service contents')
    require({entry.name for entry in (sidecar / 'service/metadata').iterdir()} ==
            {'signed-root', 'manifest.toml', 'collection.qcl'}, 'unexpected capture metadata contents')
    require({entry.name for entry in (sidecar / 'runtime').iterdir()} == {pins['runtime_version']},
            'capture contains another runtime')
    entries = list(sidecar.rglob('*'))
    require(len(entries) <= 8192, 'capture sidecar inventory exceeds bounds')
    total = 0
    for entry in entries:
        require(not entry.is_symlink(), 'capture sidecar contains a redirected entry')
        metadata = entry.stat()
        require(stat.S_ISDIR(metadata.st_mode) or stat.S_ISREG(metadata.st_mode),
                'capture sidecar contains a special file')
        if stat.S_ISREG(metadata.st_mode):
            total += metadata.st_size
    require(total <= 3 * 1024 ** 3, 'capture sidecar size exceeds bounds')
    payload_files = {str(entry.relative_to(sidecar / 'service/payload'))
                     for entry in (sidecar / 'service/payload').rglob('*') if entry.is_file()}
    require(payload_files == {'bin/tilo-home-export'}, 'capture payload contains unexpected files')
    package = sidecar / 'service/home-export.tapp'
    regular(package, 256 * 1024 * 1024)
    invoke([str(packer), 'verify', '--trusted-key', pins['service_publisher'], str(package)])
    metadata = {}
    with tarfile.open(package) as archive:
        for name, limit in (('tapp.toml', 4096), ('signed-root', 173),
                            ('manifest.toml', 1024 * 1024), ('collection.qcl', 16 * 1024 * 1024)):
            matches = [entry for entry in archive.getmembers() if entry.name == name]
            require(len(matches) == 1 and matches[0].isfile() and matches[0].size <= limit,
                    'capture package metadata is missing, duplicate or outside bounds')
            metadata[name] = archive.extractfile(matches[0]).read()
    header = tomllib.loads(metadata['tapp.toml'].decode())
    signed = metadata['signed-root']
    require(header['package_id'] == 'tapp:' + pins['package_hash'] and len(signed) == 173,
            'capture package hash differs')
    require(signed[44:76].hex() == pins['payload_root_hash']
            and hashlib.sha256(metadata['manifest.toml']).hexdigest() == pins['manifest_sha256'],
            'capture manifest or payload identity differs')
    manifest = tomllib.loads(metadata['manifest.toml'].decode())
    require(manifest['package']['id'] == pins['package_id']
            and manifest['package']['kind'] == 'service'
            and manifest['package']['runtime'] == 'tilo-runtime/' + pins['runtime_version']
            and manifest['package']['service-protocol'] == {'name': 'tilo.home-capture', 'version': 6}
            and manifest['entrypoint']['bin'] == 'bin/tilo-home-export',
            'signed capture worker contract differs')
    for name in ('signed-root', 'manifest.toml', 'collection.qcl'):
        require(regular(sidecar / 'service/metadata' / name, 16 * 1024 * 1024) == metadata[name],
                'capture extracted metadata differs from signed package')
    worker = sidecar / 'service/payload/bin/tilo-home-export'
    worker_bytes = regular(worker, 64 * 1024 * 1024)
    with tempfile.TemporaryDirectory(prefix='tilo-capture-admit-') as scratch:
        payload = Path(scratch) / 'payload'
        invoke([str(packer), 'unpack', '--dest', str(payload), str(package)])
        require(regular(payload / 'bin/tilo-home-export', 64 * 1024 * 1024) == worker_bytes,
                'capture worker differs from signed package')
    runtime = sidecar / 'runtime' / pins['runtime_version']
    bundle_bytes = regular(runtime / 'bundle.toml', 8 * 1024 * 1024)
    bundle = tomllib.loads(bundle_bytes.decode())
    require(bundle['version'] == pins['runtime_version'] and bundle['arch'] == arch
            and bundle['publisher_hex'] == pins['runtime_publisher']
            and bundle['root_hash'] == pins['runtime_root_hash'], 'capture runtime identity differs')
    invoke([str(verifier), 'verify', str(runtime), '--require-publisher', pins['runtime_publisher'],
            '--require-signature', '--require-arch', arch])
    invoke(['bash', str(ROOT / 'os/scripts/verify-runtime-bundle-closure.sh'), str(runtime), str(worker)])
    require(regular(pins_path, 16384) == original, 'capture pins changed during admission')
    # Emit the exact bytes so the production consumer's SHA-256 remains stable.
    return original


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--pins', type=Path, required=True)
    parser.add_argument('--sidecar', type=Path, required=True)
    parser.add_argument('--arch', choices=('x86_64', 'aarch64'), required=True)
    args = parser.parse_args()
    import sys
    sys.stdout.buffer.write(admit(args.pins, args.sidecar, args.arch))


if __name__ == '__main__':
    main()
