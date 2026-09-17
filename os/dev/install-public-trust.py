#!/usr/bin/env python3
"""Install validated public image trust before initramfs generation."""
import argparse
import importlib.util
import json
from pathlib import Path

spec = importlib.util.spec_from_file_location('development_trust', Path(__file__).with_name('development-trust.py'))
trust = importlib.util.module_from_spec(spec)
spec.loader.exec_module(trust)
origin_spec = importlib.util.spec_from_file_location('hardware_profile', Path(__file__).with_name('hardware_profile.py'))
origin = importlib.util.module_from_spec(origin_spec)
origin_spec.loader.exec_module(origin)
PRODUCTION_FILES = ('cosign.pub', 'tpm2-pcr-signing.public.pem')


def install(source, official, destination, mode, digest, owner, target_ref="", image_root=None):
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
        selected = origin.development_target(target_ref)
        if image_root is None:
            raise ValueError('development trust requires an explicit image root')
        root = Path(image_root)
        if destination != root / 'usr/share/tilo':
            raise ValueError('development trust destination does not match image root')
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
        owner_directory = destination / 'trust'
        owner_directory.mkdir(exist_ok=True)
        (owner_directory / 'tilo-main-registry-owner.pub').write_text(owner + '\n')
        directory = destination / 'development-trust'
        directory.mkdir()
        for name in (*trust.FILES, 'manifest.json'):
            (directory / name).write_bytes(trust.read_public(source / name))
        (directory / 'provenance.json').write_text(json.dumps(provenance, indent=2, sort_keys=True) + '\n')
        document = {'schema': 1, 'purpose': 'tilo-development',
                    'target_update_ref': target_ref, 'manifest_sha256': digest}
        (directory / 'origin.json').write_text(json.dumps(document, indent=2, sort_keys=True) + '\n')
        containers = root / 'etc/containers'
        containers.mkdir(parents=True, exist_ok=True)
        (containers / 'policy.json').write_text(json.dumps(origin.development_policy(selected['repository']), indent=2) + '\n')
        registries = containers / 'registries.d'
        registries.mkdir(exist_ok=True)
        (registries / 'tilo-development.yaml').write_text('docker:\n  "' + selected['repository'] + '":\n    use-sigstore-attachments: true\n')
        transport = containers / 'registries.conf.d'
        transport.mkdir(exist_ok=True)
        # HTTP is only permitted for the literal loopback IPv4 endpoint.
        (transport / '99-tilo-development.conf').write_text('[[registry]]\nlocation = "' + selected['registry'] + '"\ninsecure = ' + str(not selected['tls_required']).lower() + '\n')



def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('source', 'official', 'destination', 'mode'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--manifest-sha256', default='')
    parser.add_argument('--registry-owner', default='')
    parser.add_argument('--target-ref', default='')
    parser.add_argument('--image-root', type=Path)
    args = parser.parse_args()
    try:
        install(args.source, args.official, args.destination, args.mode, args.manifest_sha256, args.registry_owner, args.target_ref, args.image_root)
    except (OSError, ValueError, TypeError) as error:
        parser.exit(1, f'public trust: {error}\n')


if __name__ == '__main__':
    main()
