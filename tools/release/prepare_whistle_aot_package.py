#!/usr/bin/env python3
"""Prepare the exact qualified immutable AOT runtime for release review.

This writes a new image-layout candidate, never installs, signs, publishes or
activates it. Hashes are reviewed source constants, not caller-supplied native
code authority. Model download, licence acceptance and selection remain on the
existing signed catalogue path; model bytes are not part of this runtime.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat

ROOT = Path(__file__).resolve().parents[2]
SOURCE_COMMIT = '3c8a3e79aa3188a1a12b3aebbaa097abccceac92'
OBJECTS = {
    'libwasmtime.so': (17465504, '2d232d8bc62ece8ded8179cbb354f17729dadfdf04ff018a96cbbdaf075433dd'),
    'aot-bridge.so': (36544, 'd628553aea35bf059b9e30676c123868193aee8e59b1783f43028690f22abe34'),
    'needle.aot': (3437656, 'a74daea810fcccb4053ff8639f7bae9b7010fd9d9e1ad80a0deecc17c2cb3d26'),
}
NOTICES = {
    'Wasmtime': (12243, '268872b9816f90fd8e85db5a28d33f8150ebb8dd016653fb39ef1f94f2686bc5'),
    'Whistle': (11358, 'cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30'),
}
MODULE_SHA = 'c43f48e11f302087250d1e406024956781cd2f02595a5e343b3d7ddd5ef707fa'
MODEL_SHA = 'b6e02f048568ac5d01a2042556c658061e699acbc0aa2a1439f52f3d461dffeb'


def verified(path, size, digest):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    with os.fdopen(descriptor, 'rb') as stream:
        metadata = os.fstat(stream.fileno())
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid()
                or metadata.st_mode & 0o022 or metadata.st_size != size):
            raise ValueError('owned immutable-size package input required')
        data = stream.read(size + 1)
    if len(data) != size or hashlib.sha256(data).hexdigest() != digest:
        raise ValueError('package input differs from reviewed source pin')
    return data


def prepare(runtime, notices, destination):
    ledger = json.loads((ROOT / 'docs/architecture/dictation-whistle-aot-review.json').read_text())
    receipt = ledger['runtime']
    if (receipt['source_commit'] != SOURCE_COMMIT or receipt['version'] != '49.0.2'
            or receipt['target'] != 'x86-64-v3' or not receipt['signals_and_spectre_defaults']
            or not ledger['qualification_only'] or ledger['production_activation']):
        raise ValueError('qualified source configuration required')
    native = {}
    for name, (size, digest) in OBJECTS.items():
        path = runtime / ('factory-output/needle.aot' if name == 'needle.aot' else name)
        native[name] = verified(path, size, digest)
    notice_bytes = {name: verified(notices / name, *pin) for name, pin in NOTICES.items()}
    sources = {}
    for name in ('whistle_aot_config.hpp', 'whistle_aot_factory.cpp', 'whistle_aot_review.cpp'):
        pin = receipt['files'][name]
        sources[name] = verified(ROOT / 'tools/release/fixtures' / name,
                                 pin['bytes'], pin['sha256'])
    # Verify all inputs before creating any output. The destination must be new;
    # no production path or existing candidate is ever updated in place.
    destination.mkdir(mode=0o700, parents=False, exist_ok=False)
    library = destination / 'usr/lib64/tilo-speech/whistle/x86-64-v3'
    library.mkdir(parents=True, mode=0o755)
    licence = destination / 'usr/share/licenses/tilo-speech-whistle'
    licence.mkdir(parents=True, mode=0o755)
    provenance = destination / 'usr/share/tilo-speech/whistle'
    provenance.mkdir(parents=True, mode=0o755)
    # pathlib's parents=True applies its mode only to the leaf. Image admission
    # rejects writable ancestors, so normalise every newly created image parent
    # even when the release operator has a group-writable umask.
    for path in destination.rglob('*'):
        if path.is_dir():
            path.chmod(0o755)
    for name, data in native.items():
        path = library / name
        path.write_bytes(data)
        path.chmod(0o644)
    for name, data in notice_bytes.items():
        path = licence / (name + '.LICENSE')
        path.write_bytes(data)
        path.chmod(0o644)
    for name, data in sources.items():
        path = provenance / name
        path.write_bytes(data)
        path.chmod(0o644)
    manifest = {
        'schema': 1, 'candidate_only': True, 'installed': False,
        'target': 'x86-64-v3', 'runtime_source_commit': SOURCE_COMMIT,
        'runtime_version': '49.0.2', 'module_sha256': MODULE_SHA,
        'model_sha256': MODEL_SHA, 'model_included': False,
        'files': {name: {'size_bytes': size, 'sha256': digest}
                  for name, (size, digest) in OBJECTS.items()},
        'notices': {name: {'size_bytes': size, 'sha256': digest}
                    for name, (size, digest) in NOTICES.items()},
        'source_build': receipt, 'factory': ledger['factory'],
        'admission_required': 'Existing immutable image provenance; root-owned package files; '
                              'exact native pins; compatible CPU; signed model catalogue and '
                              'participant licence acceptance. This candidate grants none.',
    }
    path = provenance / 'manifest.json'
    path.write_text(json.dumps(manifest, indent=2) + '\n')
    path.chmod(0o644)
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runtime-dir', required=True, type=Path)
    parser.add_argument('--notice-dir', required=True, type=Path,
                        help='Owned pinned notices named Wasmtime and Whistle')
    parser.add_argument('destination', type=Path)
    args = parser.parse_args()
    if os.geteuid() == 0 or os.geteuid() != os.getuid():
        parser.error('unprivileged release preparation required')
    prepare(args.runtime_dir, args.notice_dir, args.destination)
    print('Exact runtime candidate prepared; admission, signing and activation remain separate.')


if __name__ == '__main__':
    main()
