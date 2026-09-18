#!/usr/bin/env python3
"""Offline historical-artifact rehearsal, not an installation/trust-policy verifier.

Requires Python 3.11+, blake3 and cryptography, Bash and GNU tar. Never builds,
downloads, publishes, or launches payloads. Run in an isolated checkout: work must
be a NEW directory below its dist/. Receipts describe historical bytes only.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import stat
import struct
import subprocess
import tarfile
import tomllib
import zipfile

from blake3 import blake3
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey


def require(condition, message):
    if not condition:
        raise ValueError(message)


def safe_path(name):
    p = PurePosixPath(name)
    require(bool(name) and not p.is_absolute() and '\\' not in name
            and ':' not in name and '\x00' not in name
            and all(part not in ('', '.', '..') for part in name.split('/')),
            f'unsafe archive path: {name!r}')
    return p


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def extract(archive, dest, expected_size):
    with zipfile.ZipFile(archive) as z:
        entries = z.infolist()
        require(len(entries) <= 4096, 'too many ZIP entries')
        require(sum(i.file_size for i in entries) == expected_size, 'artifact size mismatch')
        names = set()
        for i in entries:
            safe_path(i.filename)
            mode = i.external_attr >> 16
            require(not i.is_dir() and stat.S_IFMT(mode) in (0, stat.S_IFREG),
                    'only regular ZIP files permitted')
            require(i.filename not in names, 'duplicate ZIP entry')
            names.add(i.filename)
        dest.mkdir()
        for i in entries:
            target = dest / i.filename
            target.parent.mkdir(parents=True, exist_ok=True)
            with z.open(i) as source, target.open('xb') as output:
                while chunk := source.read(1024 * 1024):
                    output.write(chunk)
    checks = {}
    for line in (dest / 'SHA256SUMS').read_text().splitlines():
        digest, name = line.split('  ', 1)
        safe_path(name)
        require(name not in checks and len(digest) == 64, 'invalid checksum manifest')
        checks[name] = digest
    require(set(checks) == names - {'SHA256SUMS'}, 'checksum coverage mismatch')
    for name, digest in checks.items():
        require(sha(dest / name) == digest, f'SHA256 mismatch: {name}')


def verify_qpkg(path, app):
    with tarfile.open(path, 'r:') as archive:
        members = archive.getmembers()
        require(len(members) <= 4096 and sum(m.size for m in members) <= path.stat().st_size,
                'excessive qpkg expansion')
        names = [m.name for m in members]
        require(len(names) == len(set(names)), 'duplicate qpkg member')
        for m in members:
            safe_path(m.name)
            require(m.isfile() and not m.issparse(), 'nonregular or sparse qpkg member')
        blobs = {m.name: archive.extractfile(m).read() for m in members}
    root = blobs['signed-root']
    require(len(root) == 173 and root[:8] == b'TLOPKG\0\0'
            and struct.unpack('<I', root[8:12])[0] == 1 and root[76] == 1,
            'unsupported signed root')
    Ed25519PublicKey.from_public_bytes(root[77:109]).verify(root[109:], root[:109])
    header = tomllib.loads(blobs['qpkg.toml'].decode())
    require(header['format'] == 1 and header['package_id'] == 'qpkg:' + blake3(root).hexdigest(),
            'package identity mismatch')
    require(blake3(blobs['manifest.toml']).digest() == root[12:44], 'manifest hash mismatch')
    collection = blobs['collection.qcl']
    require(blake3(collection).digest() == root[44:76], 'collection hash mismatch')
    require(collection[:8] == b'TLOCOL\0\0', 'collection format mismatch')
    count = struct.unpack_from('<I', collection, 8)[0]
    pos, paths, hashes = 12, [], set()
    for _ in range(count):
        length = struct.unpack_from('<I', collection, pos)[0]
        pos += 4
        name = collection[pos:pos + length].decode()
        safe_path(name)
        pos += length
        digest = collection[pos:pos + 32].hex()
        pos += 32
        require(len(digest) == 64 and blake3(blobs['blobs/' + digest]).hexdigest() == digest,
                'payload hash mismatch')
        paths.append(name)
        hashes.add('blobs/' + digest)
    require(pos == len(collection) and paths == sorted(set(paths)), 'invalid collection ordering')
    require(set(blobs) == hashes | {'qpkg.toml', 'signed-root', 'manifest.toml', 'collection.qcl'},
            'unexpected qpkg payload')
    manifest = tomllib.loads(blobs['manifest.toml'].decode())
    require(manifest['package']['id'] == app['id']
            and manifest['package']['version'] == app['version']
            and manifest['package']['runtime'] == app['runtime'], 'app manifest mismatch')
    require(manifest['entrypoint']['bin'] in paths, 'missing entrypoint')
    return {'package_id': header['package_id'], 'publisher': root[77:109].hex()}


def verify_runtime(directory, architecture):
    bundles = list(directory.glob('tilo-runtime/*/bundle.toml'))
    require(len(bundles) == 1, 'expected one runtime')
    path = bundles[0]
    data = tomllib.loads(path.read_text())
    require(data['arch'] == architecture and data['version'] == path.parent.name, 'runtime identity mismatch')
    publisher = (directory / 'runtime-bundle-publisher.hex').read_text().strip()
    require(publisher == data['publisher_hex'], 'runtime publisher mismatch')
    paths, root = [], blake3()
    for f in data['files']:
        safe_path(f['path'])
        payload = path.parent / f['path']
        require(payload.stat().st_size == f['size'] and blake3(payload.read_bytes()).hexdigest() == f['hash'],
                'runtime payload mismatch')
        paths.append(f['path'])
        root.update((f['path'] + '\t' + f['hash'] + '\n').encode())
    require(paths == sorted(set(paths)), 'runtime ordering mismatch')
    require({p.relative_to(path.parent).as_posix() for p in path.parent.rglob('*') if p.is_file()}
            == set(paths) | {'bundle.toml'}, 'runtime payload coverage mismatch')
    require(root.hexdigest() == data['root_hash'], 'runtime root mismatch')
    message = blake3(b'tilo-runtime-bundle-v1\0' + data['version'].encode() + data['root_hash'].encode()).digest()
    Ed25519PublicKey.from_public_bytes(bytes.fromhex(publisher)).verify(bytes.fromhex(data['signature_hex']), message)
    return {'version': data['version'], 'root_hash': data['root_hash'], 'publisher': publisher}


def replay(archive, work, source, expected_size, epoch, run_id, artifact_id):
    repo = Path(__file__).resolve().parents[1]
    work = work.resolve()
    require(work.is_relative_to(repo / 'dist') and not work.exists(), 'work must be new and below checkout dist/')
    require(expected_size > 0 and epoch >= 0, 'invalid size or source epoch')
    # Conservative estimate for this rehearsal, not an operating-system quota:
    # compressed input, expanded input/output, worst-case uncompressed output
    # tar, plus 200 MiB for checkout/dependencies and filesystem metadata.
    require(archive.stat().st_size + 3 * expected_size + 200 * 1024**2 < 2 * 1024**3,
            'rehearsal exceeds 2 GiB footprint budget')
    work.mkdir(parents=True)
    original = work / 'input'
    extract(archive, original, expected_size)
    index = json.loads((original / 'index.json').read_text())
    require(index['format'] == 1 and index['source'] == source, 'source identity mismatch')
    runtime = verify_runtime(original / 'runtime', index['architecture'])
    packages = {}
    for app in index['apps']:
        safe_path(app['file'])
        package = original / 'qpkg' / app['file']
        require(app['file'] not in packages and sha(package) == app['sha256'], 'app index mismatch')
        require(app['runtime'] == 'tilo-runtime/' + runtime['version'], 'runtime requirement mismatch')
        packages[app['file']] = verify_qpkg(package, app)
    require(set(packages) == {p.name for p in (original / 'qpkg').iterdir()}, 'qpkg inventory mismatch')
    plan = work / 'plan.json'
    plan.write_text(json.dumps({'catalog': index['apps']}))
    env = dict(os.environ, TILO_APP_COLLECTION_PLAN=str(plan), TILO_APP_ARCH=index['architecture'],
               TILO_APP_QPKG_DIR=str(original / 'qpkg'), TILO_APP_RUNTIME_DIR=str(original / 'runtime'),
               TILO_APP_COLLECTION_OUT=str(work / 'output'), TILO_APP_SOURCE=source, SOURCE_DATE_EPOCH=str(epoch))
    env.pop('GITHUB_SHA', None)
    results = []
    for _ in range(2):
        subprocess.run(['bash', str(repo / 'dist/apps/assemble-collection.sh')], env=env, check=True)
        results.append({p.relative_to(work / 'output').as_posix(): sha(p)
                        for p in (work / 'output').rglob('*') if p.is_file()})
    require(results[0] == results[1], 'assembly is nondeterministic')
    receipt = {'schema': 1, 'run_id': run_id, 'artifact_id': artifact_id, 'source': source,
               'assembler_sha256': {name: sha(repo / name) for name in
                                    ('dist/apps/assemble-collection.sh', 'tools/assemble_app_collection.py')},
               'architecture': index['architecture'], 'archive_sha256': sha(archive),
               'archive_bytes': archive.stat().st_size, 'expanded_bytes': expected_size,
               'runtime': runtime, 'packages': packages, 'output_sha256': results[0],
               'replays': 2, 'deterministic': True, 'source_date_epoch': epoch,
               'qualification': 'Historical artifact cryptographic integrity and current assembler compatibility only; '
               'no current-source, native execution, ABI, publisher trust/revocation, or publication qualification.'}
    (work / 'receipt.json').write_text(json.dumps(receipt, indent=2, sort_keys=True) + '\n')
    return receipt


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', type=Path, required=True)
    parser.add_argument('--work', type=Path, required=True)
    parser.add_argument('--source', required=True)
    parser.add_argument('--expected-size', type=int, required=True)
    parser.add_argument('--epoch', type=int, required=True)
    parser.add_argument('--run-id', type=int, required=True)
    parser.add_argument('--artifact-id', type=int, required=True)
    args = parser.parse_args()
    result = replay(args.archive, args.work, args.source, args.expected_size, args.epoch, args.run_id, args.artifact_id)
    print(json.dumps({'packages_verified': len(result['packages']), 'deterministic': result['deterministic']}))
