#!/usr/bin/env python3
"""Verify rebound initramfs bytes before removing the development signing block.

This filesystem gate does not build an initramfs, authenticate a registry image,
sign a loader, seal an image, or prove bootability. The caller must hold a
disposable working-container mount and the marker hash returned by its own
validated trust replacement operation.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import tempfile

HERE = Path(__file__).resolve().parent


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, HERE / filename)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


rebind = module('complete_rebind', 'rebind-development-trust.py')
hardware = rebind.hardware
trust = rebind.installer.trust
PENDING = hardware.DEVELOPMENT_PATH / 'initramfs-rebind-required.json'
VERIFIED = hardware.DEVELOPMENT_PATH / 'initramfs-rebind-verified.json'
FIELDS = {'schema', 'purpose', 'kernel_version', 'generation', 'architecture',
          'old_manifest_sha256', 'new_manifest_sha256', 'old_target_update_ref',
          'new_target_update_ref', 'original_kernel_sha256', 'original_initramfs_sha256',
          'old_pcr_public_sha256', 'new_pcr_public_sha256'}


def bounded(path, limit):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        with os.fdopen(descriptor, 'rb', closefd=False) as stream:
            result = stream.read(limit + 1)
        if len(result) > limit:
            raise ValueError('completion input exceeds size limit')
        return result
    finally:
        os.close(descriptor)


def file_hash(path, limit=512 * 1024 * 1024):
    if not 0 < path.stat().st_size <= limit:
        raise ValueError('completion artifact size is invalid')
    with path.open('rb') as stream:
        digest = hashlib.sha256()
        count = 0
        while chunk := stream.read(1024 * 1024):
            count += len(chunk)
            if count > limit:
                raise ValueError('completion artifact grew beyond limit')
            digest.update(chunk)
    return digest.hexdigest()


def verify_artifact(image, key):
    proof = module('complete_initramfs_trust', 'initramfs-trust.py').verify(image, key)
    audit = module('complete_initramfs_inventory', 'installed_material_audit.py')

    class DriverInventory(audit.Scanner):
        def entry(self, name, mode, size, stream):
            # Reuse the bounded CPIO/squashfs reader without applying unrelated
            # installed-state policy to the initramfs under construction.
            if re.search(r'(?:^|/)xfs\.ko(?:\.(?:xz|gz|zst))?$', name):
                raise ValueError('omitted filesystem driver present in rebound initramfs')
            self.entries += 1
            self.bytes += size
            if self.entries > audit.MAX_ENTRIES or self.bytes > audit.MAX_TOTAL:
                raise ValueError('initramfs driver inventory exceeds bounds')
            remaining = size
            while remaining:
                if audit.time.monotonic() > self.deadline:
                    raise ValueError('initramfs driver inventory exceeded deadline')
                remaining -= len(audit.archive.exact(stream, min(remaining, audit.archive.CHUNK)))

    inventory = DriverInventory().initramfs(bounded(image, 512 * 1024 * 1024))
    if inventory['sha256'] != proof['initramfs_sha256']:
        raise ValueError('initramfs changed between trust and driver verification')
    proof['omitted_kernel_drivers_verified'] = ['xfs']
    return proof


def complete(root, *, disposable, marker_sha256, public_bundle, manifest_sha256,
             target, architecture, generation):
    root = Path(root)
    if (disposable is not True or not root.is_absolute() or root.resolve() != root
            or root == Path('/') or os.getuid() != 0 or architecture != platform.machine()
            or architecture not in rebind.retention.source.ARCHITECTURES
            or type(generation) is not int or not 1 <= generation < 2**64
            or not re.fullmatch('[0-9a-f]{64}', marker_sha256)):
        raise ValueError('explicit native disposable root and independent marker hash required')
    hardware.trusted_path(root, Path(), regular=False)
    trust.validate(public_bundle, manifest_sha256)
    expected_key = trust.read_public(Path(public_bundle) / 'tpm2-pcr-signing.public.pem')
    # The independently pinned manifest also binds the bytes read after validation.
    manifest = json.loads(trust.read_public(Path(public_bundle) / 'manifest.json'),
                          object_pairs_hook=trust.unique_object)
    if hashlib.sha256(expected_key).hexdigest() != manifest['sha256']['tpm2-pcr-signing.public.pem']:
        raise ValueError('selected PCR public input changed')
    pending = hardware.trusted_path(root, PENDING, regular=True)
    raw = bounded(pending, 16384)
    if hashlib.sha256(raw).hexdigest() != marker_sha256:
        raise ValueError('pending marker differs from the replacement operation')
    marker = json.loads(raw, object_pairs_hook=trust.unique_object)
    if (not isinstance(marker, dict) or set(marker) != FIELDS
            or type(marker['schema']) is not int or marker['schema'] != 1
            or marker['purpose'] != 'tilo-development-initramfs-rebind-required'
            or marker['architecture'] != architecture or type(marker['generation']) is not int
            or marker['generation'] != generation or marker['new_manifest_sha256'] != manifest_sha256
            or marker['new_target_update_ref'] != target):
        raise ValueError('pending marker differs from independent completion inputs')
    for field in FIELDS:
        if field.endswith('_sha256') and not re.fullmatch('[0-9a-f]{64}', marker[field]):
            raise ValueError('invalid marker digest')
    hardware.development_target(target, architecture)
    if hardware.development_origin(root) != target:
        raise ValueError('new image origin or public trust differs')
    installed_manifest = hardware.trusted_path(root, hardware.DEVELOPMENT_PATH / 'manifest.json', regular=True)
    if hashlib.sha256(bounded(installed_manifest, 65536)).hexdigest() != manifest_sha256:
        raise ValueError('new image manifest differs')
    profile = hardware.read_profile(root)
    if profile is not None and (profile['architecture'] != architecture or profile['target_update_ref'] != target):
        raise ValueError('new image profile differs')
    generation_path = hardware.trusted_path(root, Path('usr/lib/tilo/generation'), regular=True)
    if bounded(generation_path, 32) not in (str(generation).encode(), f'{generation}\n'.encode()):
        raise ValueError('image generation differs')
    for relative in ('boot/EFI/Linux', 'boot/tilo-target', 'kernel'):
        candidate = root / relative
        if candidate.exists() or candidate.is_symlink():
            raise ValueError('completed or split rootfs is forbidden')
    loader_dir = hardware.trusted_path(root, Path('usr/lib/systemd/boot/efi'), regular=False)
    if any(path.name.endswith('.signed') for path in loader_dir.iterdir()):
        raise ValueError('signed rootfs is forbidden')
    kver = rebind.retention.raw_kernel_pair(root)
    if kver != marker['kernel_version']:
        raise ValueError('kernel version changed during regeneration')
    kernel = hardware.trusted_path(root, Path('usr/lib/modules') / kver / 'vmlinuz', regular=True)
    initramfs = hardware.trusted_path(root, Path('usr/lib/modules') / kver / 'initramfs.img', regular=True)
    if file_hash(kernel) != marker['original_kernel_sha256']:
        raise ValueError('kernel bytes changed during regeneration')
    if hashlib.sha256(expected_key).hexdigest() != marker['new_pcr_public_sha256']:
        raise ValueError('PCR key differs from replacement operation')
    destination = root / VERIFIED
    if destination.exists() or destination.is_symlink():
        hardware.trusted_path(root, VERIFIED, regular=True)
    proof = verify_artifact(initramfs, expected_key)
    if (proof.get('initramfs_sha256') != file_hash(initramfs)
            or proof.get('pcr_public_sha256') != hashlib.sha256(expected_key).hexdigest()):
        raise ValueError('artifact verification does not bind the current bytes')
    # Do not clear a marker changed during a potentially long artifact inspection.
    if (bounded(pending, 16384) != raw
            or file_hash(kernel) != marker['original_kernel_sha256']
            or bounded(generation_path, 32) not in (str(generation).encode(), f'{generation}\n'.encode())
            or hardware.read_profile(root) != profile
            or hardware.development_origin(root) != target
            or hashlib.sha256(bounded(installed_manifest, 65536)).hexdigest() != manifest_sha256):
        raise ValueError('working image changed during verification')
    receipt = {'schema': 1, 'purpose': 'tilo-development-initramfs-trust-verified',
               'pending_marker_sha256': marker_sha256, 'kernel_version': kver,
               'kernel_sha256': marker['original_kernel_sha256'], 'generation': generation,
               'manifest_sha256': manifest_sha256, 'target_update_ref': target,
               'artifact': proof, 'installable': False, 'boot_proven': False}
    descriptor, staged = tempfile.mkstemp(prefix='.verified-initramfs-', dir=destination.parent)
    try:
        with os.fdopen(descriptor, 'w') as stream:
            json.dump(receipt, stream, sort_keys=True, indent=2)
            stream.write('\n')
        os.chmod(staged, 0o644)
        os.replace(staged, destination)
        pending.unlink()
    finally:
        Path(staged).unlink(missing_ok=True)
    try:
        rebind.retention.source.mounted_identity(root, target, manifest_sha256, architecture)
    except BaseException:
        # A final validation failure must never turn a pending working root
        # into an accepted one. The caller must discard failed working roots.
        with pending.open('xb') as stream:
            stream.write(raw)
        pending.chmod(0o644)
        raise
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--disposable-working-root', required=True, type=Path)
    parser.add_argument('--public-bundle', required=True, type=Path)
    for field in ('marker-sha256', 'manifest-sha256', 'target', 'architecture'):
        parser.add_argument('--' + field, required=True)
    parser.add_argument('--generation', required=True, type=int)
    args = parser.parse_args()
    try:
        print(json.dumps(complete(args.disposable_working_root, disposable=True,
            marker_sha256=args.marker_sha256, public_bundle=args.public_bundle,
            manifest_sha256=args.manifest_sha256, target=args.target,
            architecture=args.architecture, generation=args.generation), sort_keys=True))
    except (OSError, ValueError, TypeError, KeyError) as error:
        parser.exit(1, f'initramfs trust completion rejected: {type(error).__name__}\n')


if __name__ == '__main__':
    main()
