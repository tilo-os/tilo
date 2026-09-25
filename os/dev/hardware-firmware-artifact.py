#!/usr/bin/env python3
"""Import independently pinned board firmware without executing artifact code."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tempfile

import hardware_profile as hardware

spec = importlib.util.spec_from_file_location('firmware_preparer', Path(__file__).with_name('prepare-hardware-profile.py'))
preparer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(preparer)
ARCHITECTURES = {'aarch64': 'arm64', 'x86_64': 'amd64'}


def command(*arguments):
    environment = {'PATH': '/usr/local/bin:/usr/bin:/bin', 'HOME': '/root', 'LANG': 'C.UTF-8'}
    if os.environ.get('REGISTRY_AUTH_FILE'):
        environment['REGISTRY_AUTH_FILE'] = os.environ['REGISTRY_AUTH_FILE']
    result = subprocess.run(['podman', '--remote=false', *arguments], env=environment,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if result.returncode:
        # Podman does not receive the registry token in argv or env. Keep its
        # last diagnostic line so CI can distinguish auth, network and storage
        # failures, while removing URL userinfo and auth header values.
        detail = result.stderr.decode('utf-8', errors='replace').strip().splitlines()
        detail = detail[-1][:1000] if detail else 'no diagnostic'
        detail = re.sub(r'(?i)(https?://)[^/@\s]+@', r'\1[redacted]@', detail)
        detail = re.sub(r'(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/-]+=*',
                        r'\1 [redacted]', detail)
        detail = re.sub(r'(?i)\b(authorization|password|token)(\s*[:=]\s*)\S+',
                        r'\1\2[redacted]', detail)
        raise ValueError(f'firmware Podman {arguments[0]} failed (exit {result.returncode}): {detail}')
    return result.stdout.decode('utf-8').strip()


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate firmware JSON field')
        result[key] = value
    return result


def payload(directory, definition, expected_manifest):
    names = {'manifest.json', *definition['firmware']}
    if directory.is_symlink() or not directory.is_dir() or {p.name for p in directory.iterdir()} != names:
        raise ValueError('firmware directory must contain exactly the declared payload')
    manifest_path = preparer.checked_path(directory, Path('manifest.json'), required=True)
    if manifest_path.stat().st_size > 16384:
        raise ValueError('firmware manifest is too large')
    raw = manifest_path.read_bytes()
    if hashlib.sha256(raw).hexdigest() != expected_manifest:
        raise ValueError('firmware manifest independent digest differs')
    manifest = json.loads(raw, object_pairs_hook=unique_object)
    if (not isinstance(manifest, dict) or set(manifest) != {'schema_version', 'profile', 'files'}
            or type(manifest['schema_version']) is not int or manifest['schema_version'] != 1
            or manifest['profile'] != definition['id']
            or not isinstance(manifest['files'], dict)
            or set(manifest['files']) != set(definition['firmware'])):
        raise ValueError('firmware manifest differs from hardware definition')
    contents = {'manifest.json': raw}
    for name, digest in manifest['files'].items():
        if not isinstance(digest, str) or not re.fullmatch('[0-9a-f]{64}', digest):
            raise ValueError('invalid firmware digest')
        path = preparer.checked_path(directory, Path(name), required=True)
        if not 0 < path.stat().st_size <= 128 * 1024 * 1024:
            raise ValueError('firmware file must be nonempty and bounded')
        data = path.read_bytes()
        if hashlib.sha256(data).hexdigest() != digest:
            raise ValueError(f'firmware hash differs: {name}')
        contents[name] = data
    return contents, manifest['files']


def stage(repo, profile, contents):
    parent = repo / 'os/hardware/firmware'
    # Staging is in a trusted root-run checkout, never through redirected paths.
    for path in (parent, *parent.parents):
        if path.is_symlink():
            raise ValueError('firmware staging ancestry contains a symlink')
    if not parent.is_dir():
        raise ValueError('firmware staging parent is missing')
    destination = parent / profile
    if destination.exists() or destination.is_symlink():
        if destination.is_symlink() or not destination.is_dir() or {p.name for p in destination.iterdir()} != set(contents):
            raise ValueError('existing firmware staging differs')
        for name, data in contents.items():
            path = preparer.checked_path(destination, Path(name), required=True)
            if path.stat().st_size != len(data) or path.read_bytes() != data:
                raise ValueError('existing firmware staging differs')
        return
    with tempfile.TemporaryDirectory(prefix='.firmware-import-', dir=parent) as temporary:
        prepared = Path(temporary) / profile
        prepared.mkdir(mode=0o755)
        for name, data in contents.items():
            target = prepared / name
            target.write_bytes(data)
            target.chmod(0o644)
        if destination.exists() or destination.is_symlink():
            raise ValueError('firmware staging appeared during import')
        prepared.rename(destination)


def import_artifact(args):
    if sys.platform != 'linux' or os.getuid() != 0 or os.geteuid() != 0 or platform.machine() != args.architecture:
        raise ValueError('firmware import requires native root Linux')
    if args.architecture not in ARCHITECTURES:
        raise ValueError('unsupported firmware architecture')
    if any(os.environ.get(key) for key in ('CONTAINER_HOST', 'CONTAINER_CONNECTION', 'CONTAINER_SSHKEY', 'DOCKER_HOST')):
        raise ValueError('remote container configuration is unsupported')
    if not re.fullmatch(r'code\.podesta\.ai/qos/[a-z0-9][a-z0-9._-]*@sha256:[0-9a-f]{64}', args.reference):
        raise ValueError('firmware requires an immutable registry reference')
    if not re.fullmatch('[0-9a-f]{64}', args.manifest_sha256):
        raise ValueError('firmware requires an independent manifest SHA256')
    if not args.repo.is_absolute() or '..' in args.repo.parts:
        raise ValueError('firmware repository path must be absolute')
    definition = hardware.load_definition(args.profile, args.repo / 'os/hardware/profiles')
    if definition['architecture'] != args.architecture:
        raise ValueError('firmware profile architecture differs')
    command('pull', '--platform=linux/' + ARCHITECTURES[args.architecture], '--tls-verify=true', args.reference)
    images = json.loads(command('image', 'inspect', args.reference), object_pairs_hook=unique_object)
    if not isinstance(images, list) or len(images) != 1 or not isinstance(images[0], dict):
        raise ValueError('expected exactly one firmware image')
    info = images[0]
    identity = info.get('Id', '')
    if (info.get('Os') != 'linux' or info.get('Architecture') != ARCHITECTURES[args.architecture]
            or info.get('Digest') != args.reference.split('@')[1]
            or args.reference not in info.get('RepoDigests', [])
            or not isinstance(identity, str) or not re.fullmatch(r'(?:sha256:)?[0-9a-f]{64}', identity)):
        raise ValueError('firmware image identity or architecture differs')
    root_text = command('image', 'mount', identity)
    try:
        root = Path(root_text)
        if not root.is_absolute() or '\n' in root_text or root.is_symlink() or {p.name for p in root.iterdir()} != {'firmware'}:
            raise ValueError('firmware artifact must contain only /firmware')
        contents, hashes = payload(root / 'firmware', definition, args.manifest_sha256)
    finally:
        command('image', 'unmount', identity)
    # No staged output survives a failed unmount or failed payload validation.
    stage(args.repo, args.profile, contents)
    return {'schema': 1, 'purpose': 'tilo-hardware-firmware-import', 'reference': args.reference,
            'manifest_sha256': args.manifest_sha256, 'profile': args.profile,
            'architecture': args.architecture, 'firmware_sha256': hashes}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=['import'])
    for name in ('reference', 'manifest-sha256', 'profile', 'architecture'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--repo', type=Path, required=True)
    try:
        print(json.dumps(import_artifact(parser.parse_args()), sort_keys=True))
    except (ValueError, OSError, TypeError, KeyError) as error:
        parser.exit(1, f'firmware import: {error}\n')


if __name__ == '__main__':
    main()
