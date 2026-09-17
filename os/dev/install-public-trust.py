#!/usr/bin/env python3
"""Install validated public image trust before initramfs generation."""
import argparse
import importlib.util
import json
from pathlib import Path

spec = importlib.util.spec_from_file_location('development_trust', Path(__file__).with_name('development-trust.py'))
trust = importlib.util.module_from_spec(spec)
spec.loader.exec_module(trust)
PRODUCTION_FILES = ('cosign.pub', 'tpm2-pcr-signing.public.pem')


def install(source, official, destination, mode, digest, owner):
    source, official, destination = map(Path, (source, official, destination))
    if mode == 'production':
        if digest or {p.name for p in source.iterdir()} != set(PRODUCTION_FILES):
            raise ValueError('production trust requires only the tracked production keys')
        inputs = {name: trust.read_public(source / name) for name in PRODUCTION_FILES}
        if any(data != trust.read_public(official / name) for name, data in inputs.items()):
            raise ValueError('production public keys differ from tracked keys')
        provenance = None
    elif mode == 'development':
        if not digest:
            raise ValueError('development trust requires an independently pinned manifest digest')
        provenance = trust.validate(source, digest)
        if owner != provenance['identities']['registry-owner.pub']:
            raise ValueError('registry owner differs from development bundle')
        inputs = {name: trust.read_public(source / name) for name in PRODUCTION_FILES}
    else:
        raise ValueError('unknown public trust mode')
    destination.mkdir(parents=True, exist_ok=True)
    for name, data in inputs.items():
        (destination / name).write_bytes(data)
    if provenance:
        directory = destination / 'development-trust'
        directory.mkdir()
        for name in (*trust.FILES, 'manifest.json'):
            (directory / name).write_bytes(trust.read_public(source / name))
        (directory / 'provenance.json').write_text(json.dumps(provenance, indent=2, sort_keys=True) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('source', 'official', 'destination', 'mode'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--manifest-sha256', default='')
    parser.add_argument('--registry-owner', default='')
    args = parser.parse_args()
    try:
        install(args.source, args.official, args.destination, args.mode, args.manifest_sha256, args.registry_owner)
    except (OSError, ValueError, TypeError) as error:
        parser.exit(1, f'public trust: {error}\n')


if __name__ == '__main__':
    main()
