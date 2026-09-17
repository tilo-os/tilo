#!/usr/bin/env python3
"""Authenticate a quarantined raw development rootfs without executing image code.

The held image mount is a verified source, never a writable transform target.
Consumers must create their own disposable derivative. This is not boot proof.
"""
import argparse
from contextlib import contextmanager
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import tempfile

spec = importlib.util.spec_from_file_location('import_retention', Path(__file__).with_name('retain-development-rootfs.py'))
retention = importlib.util.module_from_spec(spec)
spec.loader.exec_module(retention)
rootfs, source, hardware = retention.rootfs, retention.source, retention.hardware


def retained_identity(receipt, args):
    expected = {'schema', 'purpose', 'image', 'handoff', 'handoff_sha256',
                'hardware_profile', 'published', 'installable', 'sealed',
                'kernel_version', 'layer_private_material_audit', 'completion_required'}
    if not isinstance(receipt, dict) or set(receipt) != expected:
        raise ValueError('unexpected retained rootfs receipt schema')
    constants = {'schema': 1, 'purpose': 'tilo-retained-development-raw-rootfs',
                 'published': True, 'installable': False, 'sealed': False,
                 'layer_private_material_audit': 'not performed by this helper; required before publication',
                 'completion_required': ['rebind public trust and origin', 'regenerate initramfs',
                                         'sign loader', 'seal and sign UKIs', 'disk and VM verification']}
    if any(type(receipt[key]) is not type(value) or receipt[key] != value
           for key, value in constants.items()):
        raise ValueError('retained rootfs receipt overclaims or differs')
    reference = receipt['image']
    if not isinstance(reference, str) or not re.fullmatch(
            re.escape(retention.REPOSITORY) + r'@sha256:[0-9a-f]{64}', reference):
        raise ValueError('expected an exact quarantine repository digest')
    image_id = retention.validate_receipt(receipt['handoff'], args.revision, args.architecture,
        args.manifest_sha256, args.target, args.generation, args.source_date_epoch)
    if receipt['handoff_sha256'] != retention.receipt_digest(receipt['handoff']):
        raise ValueError('canonical handoff digest differs')
    if (not isinstance(receipt['kernel_version'], str) or not receipt['kernel_version']
            or len(receipt['kernel_version']) > 255
            or receipt['hardware_profile'] is not None and not isinstance(receipt['hardware_profile'], str)):
        raise ValueError('invalid retained filesystem metadata')
    return reference, image_id


def public_snapshot(original, destination, digest):
    original = Path(original)
    source.trust.validate(original, digest)
    destination.mkdir()
    for name in (*source.trust.FILES, 'manifest.json'):
        (destination / name).write_bytes(source.trust.read_public(original / name))
    source.trust.validate(destination, digest)


def raw_identity(root, args, receipt, scratch):
    profile = source.mounted_identity(root, args.target, args.manifest_sha256, args.architecture)
    if profile != receipt['hardware_profile']:
        raise ValueError('retained hardware profile differs')
    for relative in ('boot/EFI/Linux', 'boot/tilo-target', 'kernel'):
        path = root / relative
        if path.exists() or path.is_symlink():
            raise ValueError('completed or split rootfs is forbidden')
    loaders = hardware.trusted_path(root, Path('usr/lib/systemd/boot/efi'), regular=False)
    if any(path.name.endswith('.signed') for path in loaders.iterdir()):
        raise ValueError('signed rootfs is forbidden')
    generation = hardware.trusted_path(root, Path('usr/lib/tilo/generation'), regular=True)
    value = rootfs.loader.snapshot(generation, scratch / 'generation', 32)
    if value not in (str(args.generation).encode(), f'{args.generation}\n'.encode()):
        raise ValueError('retained generation differs')
    if retention.raw_kernel_pair(root) != receipt['kernel_version']:
        raise ValueError('retained kernel version differs')


@contextmanager
def verified_rootfs(args):
    if os.getuid() != 0 or platform.machine() != args.architecture:
        raise ValueError('rootfs import requires already-root native Linux')
    if any(os.environ.get(key) for key in ('CONTAINER_HOST', 'CONTAINER_CONNECTION',
            'CONTAINER_SSHKEY', 'DOCKER_HOST', 'COSIGN_REPOSITORY')):
        raise ValueError('remote engines and signature repository redirects are unsupported')
    with tempfile.TemporaryDirectory(prefix='tilo-import-rootfs-', dir='/var/lib') as temporary:
        scratch = Path(temporary)
        raw = rootfs.loader.snapshot(args.receipt, scratch / 'retained.json', 32768)
        receipt = json.loads(raw, object_pairs_hook=source.trust.unique_object)
        reference, image_id = retained_identity(receipt, args)
        bundle = scratch / 'public'
        public_snapshot(args.public_bundle, bundle, args.manifest_sha256)
        environment = {'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8'}
        environment.update({name: os.environ[name] for name in rootfs.LOCAL_STORAGE_ENVIRONMENT if name in os.environ})
        if os.environ.get('REGISTRY_AUTH_FILE'):
            auth = scratch / 'config.json'
            rootfs.loader.snapshot(Path(os.environ['REGISTRY_AUTH_FILE']), auth, 1024 * 1024)
            auth.chmod(0o600)
            environment.update(REGISTRY_AUTH_FILE=str(auth), DOCKER_CONFIG=str(scratch))
        manifest = source.command(['skopeo', 'inspect', '--raw', '--tls-verify=true',
                                   'docker://' + reference], environment)
        if 'sha256:' + hashlib.sha256(manifest.encode()).hexdigest() != reference.split('@')[1]:
            raise ValueError('remote manifest digest differs')
        retention.verify_manifest(manifest, image_id)
        signature = source.command(['cosign', 'verify', '--insecure-ignore-tlog=true',
                                    '--key', str(bundle / 'cosign.pub'), reference], environment)
        retention.verify_signature(signature, reference, receipt['handoff_sha256'])
        source.command(['podman', 'pull', '--tls-verify=true', '--quiet', reference], environment)
        info = rootfs.command(['podman', 'image', 'inspect', reference])
        source.image_identity(info, reference, args.architecture, args.revision)
        rootfs.identity(info, image_id, args.architecture, args.revision)
        with rootfs.mounted(image_id) as root:
            raw_identity(root, args, receipt, scratch)
            result = {'schema': 1, 'purpose': 'verified-retained-development-rootfs',
                      'image': reference, 'raw_rootfs_id': image_id,
                      'handoff_sha256': receipt['handoff_sha256'], 'handoff': receipt['handoff'],
                      'kernel_version': receipt['kernel_version'], 'hardware_profile': receipt['hardware_profile'],
                      'installable': False, 'sealed': False}
            yield source.VerifiedSource(image_id, root, bundle, result)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('receipt', 'public-bundle'):
        parser.add_argument('--' + name, required=True, type=Path)
    for name in ('revision', 'architecture', 'manifest-sha256', 'target'):
        parser.add_argument('--' + name, required=True)
    for name in ('generation', 'source-date-epoch'):
        parser.add_argument('--' + name, required=True, type=int)
    try:
        with verified_rootfs(parser.parse_args()) as verified:
            print(json.dumps(verified.receipt, sort_keys=True, indent=2))
    except (OSError, ValueError, TypeError, KeyError):
        parser.exit(1, 'retained rootfs verification failed\n')


if __name__ == '__main__':
    main()
