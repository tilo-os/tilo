#!/usr/bin/env python3
"""Install validated public image trust before initramfs generation."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import stat
import tempfile

spec = importlib.util.spec_from_file_location('development_trust', Path(__file__).with_name('development-trust.py'))
trust = importlib.util.module_from_spec(spec)
spec.loader.exec_module(trust)
origin_spec = importlib.util.spec_from_file_location('hardware_profile', Path(__file__).with_name('hardware_profile.py'))
origin = importlib.util.module_from_spec(origin_spec)
origin_spec.loader.exec_module(origin)
PRODUCTION_FILES = ('cosign.pub', 'tpm2-pcr-signing.public.pem')


def development_inputs(source, digest):
    names = (*trust.FILES, 'manifest.json')
    trust.reject_redirected_path(source)
    if {path.name for path in source.iterdir()} != set(names):
        raise ValueError('unexpected or missing public bundle files')
    bundle = {name: trust.read_public(source / name) for name in names}
    with tempfile.TemporaryDirectory(prefix='tilo-install-trust-') as temporary:
        snapshot = Path(temporary).resolve()
        for name, data in bundle.items():
            (snapshot / name).write_bytes(data)
        provenance = trust.validate(snapshot, digest)
    return provenance, bundle


def install(source, official, destination, mode, digest, owner, target_ref="", image_root=None):
    source, official, destination = map(Path, (source, official, destination))
    if mode == 'production':
        if digest or {p.name for p in source.iterdir()} != set(PRODUCTION_FILES):
            raise ValueError('production trust requires only the tracked production keys')
        inputs = {name: trust.read_public(source / name) for name in PRODUCTION_FILES}
        if any(data != trust.read_public(official / name) for name, data in inputs.items()):
            raise ValueError('production public keys differ from tracked keys')
        provenance = None
        marker = destination / 'development-trust'
        if marker.exists() or marker.is_symlink():
            raise ValueError('production trust cannot retain development trust metadata')
    elif mode == 'development':
        if not digest:
            raise ValueError('development trust requires an independently pinned manifest digest')
        selected = origin.development_target(target_ref)
        if image_root is None:
            raise ValueError('development trust requires an explicit image root')
        root = Path(image_root)
        if destination != root / 'usr/share/tilo':
            raise ValueError('development trust destination does not match image root')
        provenance, bundle = development_inputs(source, digest)
        if owner != provenance['identities']['registry-owner.pub']:
            raise ValueError('registry owner differs from development bundle')
        inputs = {name: bundle[name] for name in PRODUCTION_FILES}
    else:
        raise ValueError('unknown public trust mode')
    outputs = {destination / name: data for name, data in inputs.items()}
    if provenance:
        owner_directory = destination / 'trust'
        outputs[owner_directory / 'tilo-main-registry-owner.pub'] = (owner + '\n').encode()
        directory = destination / 'development-trust'
        if directory.exists() or directory.is_symlink():
            raise ValueError('development trust metadata already exists')
        for name, data in bundle.items():
            outputs[directory / name] = data
        outputs[directory / 'provenance.json'] = (json.dumps(provenance, indent=2, sort_keys=True) + '\n').encode()
        document = {'schema': 1, 'purpose': 'tilo-development',
                    'target_update_ref': target_ref, 'manifest_sha256': digest}
        outputs[directory / 'origin.json'] = (json.dumps(document, indent=2, sort_keys=True) + '\n').encode()
        containers = root / 'etc/containers'
        outputs[containers / 'policy.json'] = (json.dumps(origin.development_policy(selected['repository']), indent=2) + '\n').encode()
        registries = containers / 'registries.d'
        outputs[registries / 'tilo-development.yaml'] = ('docker:\n  "' + selected['repository'] + '":\n    use-sigstore-attachments: true\n').encode()
        transport = containers / 'registries.conf.d'
        # HTTP is only permitted for the literal loopback IPv4 endpoint.
        outputs[transport / '99-tilo-development.conf'] = ('[[registry]]\nlocation = "' + selected['registry'] + '"\ninsecure = ' + str(not selected['tls_required']).lower() + '\n').encode()
    # Validate the entire write set before replacing keys or policy. A late
    # directory conflict must not leave a half-switched trust configuration.
    for path in outputs:
        trust.reject_redirected_path(path)
        for parent in path.parents:
            if parent.exists() and not parent.is_dir():
                raise ValueError('public trust output parent is not a directory')
        if path.exists():
            info = path.lstat()
            if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
                raise ValueError('public trust output must be a singly linked regular file')
    for path, data in outputs.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        descriptor, temporary = tempfile.mkstemp(prefix='.tilo-trust-', dir=path.parent)
        try:
            with os.fdopen(descriptor, 'wb') as stream:
                stream.write(data)
            os.chmod(temporary, 0o644)
            os.replace(temporary, path)
        finally:
            Path(temporary).unlink(missing_ok=True)



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
