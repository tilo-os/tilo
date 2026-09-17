#!/usr/bin/env python3
"""Retain a compiled, unsealed development rootfs in a quarantine repository.

The image and public handoff receipt are signed together. Neither publication
nor its receipt authorizes installation; completion must rebind trust, rebuild
the initramfs, seal, sign and verify the final image before disk preparation.
"""
import argparse
import hashlib
import importlib.util
import json
import itertools
import os
from pathlib import Path
import platform
import re
import shutil
import tempfile

import hardware_profile as hardware

spec = importlib.util.spec_from_file_location('retained_rootfs', Path(__file__).with_name('sign-development-rootfs.py'))
rootfs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rootfs)
source = rootfs.source
REPOSITORY = 'code.podesta.ai/qos/tilo-development-rootfs'
ANNOTATION = 'tilo.raw-rootfs.receipt-sha256'


def validate_receipt(receipt, revision, architecture, manifest, target, generation, source_date_epoch):
    expected = {'schema': 1, 'purpose': 'tilo-development-raw-rootfs-handoff',
                'revision': revision, 'architecture': architecture,
                'public_manifest_sha256': manifest, 'target_update_ref': target,
                'generation': generation, 'source_date_epoch': source_date_epoch,
                'installable': False, 'sealed': False,
                'published': False,
                'validation': 'captured build inputs only; independently revalidate before reuse'}
    if not isinstance(receipt, dict) or set(receipt) != set(expected) | {'raw_rootfs_id'}:
        raise ValueError('unexpected raw rootfs handoff schema')
    if any(type(receipt[key]) is not type(value) or receipt[key] != value for key, value in expected.items()):
        raise ValueError('raw rootfs handoff differs from independent build inputs')
    if (not re.fullmatch(r'[0-9a-f]{40}', revision)
            or architecture not in source.ARCHITECTURES
            or not re.fullmatch(r'[0-9a-f]{64}', manifest)
            or type(generation) is not int or not 1 <= generation < 2**64
            or not isinstance(receipt['raw_rootfs_id'], str)
            or not re.fullmatch(r'sha256:[0-9a-f]{64}', receipt['raw_rootfs_id'])
            or type(receipt['source_date_epoch']) is not int
            or not 0 <= receipt['source_date_epoch'] <= 253402300799):
        raise ValueError('invalid raw rootfs identity or generation')
    hardware.development_target(target, architecture)
    return receipt['raw_rootfs_id']


def receipt_digest(receipt):
    return hashlib.sha256(json.dumps(receipt, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def raw_kernel_pair(root):
    """Require the intact pre-split pair; this does not attest bootability."""
    modules = hardware.trusted_path(root, Path('usr/lib/modules'), regular=False)
    with os.scandir(modules) as entries:
        candidates = list(itertools.islice(entries, 2))
    if len(candidates) != 1:
        raise ValueError('raw rootfs requires exactly one kernel directory')
    relative = Path('usr/lib/modules') / candidates[0].name
    hardware.trusted_path(root, relative, regular=False)
    for name in ('vmlinuz', 'initramfs.img'):
        path = hardware.trusted_path(root, relative / name, regular=True)
        if not 0 < path.stat().st_size <= 512 * 1024 * 1024:
            raise ValueError('raw kernel and initramfs must be bounded nonempty files')
    return candidates[0].name


def verify_signing_key(key, public_key, scratch, environment):
    derived = source.command(['cosign', 'public-key', '--key', str(key)], environment)
    if not 0 < len(derived) <= 16384:
        raise ValueError('unexpected derived Cosign public key size')
    derived_path = scratch / 'derived-cosign.pub'
    derived_path.write_text(derived)
    canonical = lambda path: source.command(
        ['openssl', 'pkey', '-pubin', '-in', str(path), '-outform', 'PEM'], environment)
    if canonical(derived_path) != canonical(public_key):
        raise ValueError('external Cosign signing identity differs from the public bundle')


def verify_signature(raw, reference, handoff_digest):
    source.signature_payloads(raw, reference)
    payloads = json.loads(raw, object_pairs_hook=source.trust.unique_object)
    if any(not isinstance(payload.get('optional'), dict)
           or payload['optional'].get(ANNOTATION) != handoff_digest for payload in payloads):
        raise ValueError('rootfs signature does not bind the public handoff receipt')


def verify_manifest(raw, image_id):
    manifest = json.loads(raw, object_pairs_hook=source.trust.unique_object)
    if (not isinstance(manifest, dict) or manifest.get('schemaVersion') != 2
            or manifest.get('mediaType') != 'application/vnd.oci.image.manifest.v1+json'
            or not isinstance(manifest.get('config'), dict)
            or manifest['config'].get('digest') != image_id
            or manifest['config'].get('mediaType') != 'application/vnd.oci.image.config.v1+json'):
        raise ValueError('published manifest does not bind the validated local image configuration')


def retain(args):
    if os.getuid() != 0 or platform.machine() != args.architecture:
        raise ValueError('rootfs retention requires already-root native Linux')
    if any(os.environ.get(key) for key in ('CONTAINER_HOST', 'CONTAINER_CONNECTION',
                                         'CONTAINER_SSHKEY', 'DOCKER_HOST', 'COSIGN_REPOSITORY')):
        raise ValueError('remote engines and signature repository redirects are unsupported')
    key = rootfs.signing.trusted(args.private_key, private=True)
    if key.is_relative_to(Path(__file__).resolve().parents[2]):
        raise ValueError('development signing key must be outside the checkout')
    with tempfile.TemporaryDirectory(prefix='tilo-retained-rootfs-', dir='/var/lib') as temporary:
        scratch = Path(temporary)
        raw = rootfs.loader.snapshot(args.receipt, scratch / 'handoff.json', 16384)
        receipt = json.loads(raw, object_pairs_hook=source.trust.unique_object)
        image_id = validate_receipt(receipt, args.revision, args.architecture,
                                    args.manifest_sha256, args.target, args.generation, args.source_date_epoch)
        handoff_digest = receipt_digest(receipt)
        bundle = scratch / 'public'
        shutil.copytree(args.public_bundle, bundle, symlinks=True)
        source.trust.validate(bundle, args.manifest_sha256)
        rootfs.identity(rootfs.command(['podman', 'image', 'inspect', image_id]),
                        image_id, args.architecture, args.revision)
        with rootfs.mounted(image_id) as root:
            profile = source.mounted_identity(root, args.target, args.manifest_sha256, args.architecture)
            path = hardware.trusted_path(root, Path('usr/lib/tilo/generation'), regular=True)
            value = rootfs.loader.snapshot(path, scratch / 'generation', 32)
            if value not in (str(args.generation).encode(), f'{args.generation}\n'.encode()):
                raise ValueError('raw rootfs generation differs')
            kernel_version = raw_kernel_pair(root)
        environment = {name: os.environ[name] for name in
                       ('PATH', 'HOME', *rootfs.LOCAL_STORAGE_ENVIRONMENT, 'REGISTRY_AUTH_FILE') if name in os.environ}
        environment['COSIGN_PASSWORD'] = ''
        # Cosign and Podman consume the same explicitly supplied registry auth.
        if environment.get('REGISTRY_AUTH_FILE'):
            auth = scratch / 'config.json'
            rootfs.loader.snapshot(Path(environment['REGISTRY_AUTH_FILE']), auth, 1024 * 1024)
            auth.chmod(0o600)
            environment['REGISTRY_AUTH_FILE'] = str(auth)
            environment['DOCKER_CONFIG'] = str(scratch)
        verify_signing_key(key, bundle / 'cosign.pub', scratch, environment)
        tag = f'{REPOSITORY}:raw-{args.revision}-{image_id[7:19]}-{args.architecture}'
        digest_file = scratch / 'digest'
        source.command(['podman', 'push', '--tls-verify=true', '--quiet', '--format=oci', '--digestfile', str(digest_file),
                        image_id, 'docker://' + tag], environment)
        digest = digest_file.read_text().strip()
        if not re.fullmatch(r'sha256:[0-9a-f]{64}', digest):
            raise ValueError('registry publication returned an invalid manifest digest')
        reference = REPOSITORY + '@' + digest
        # Fetch only immutable manifest metadata; no duplicate layer download.
        # Podman manifest inspect only handles indexes, not single OCI images.
        raw_manifest = source.command(['skopeo', 'inspect', '--raw', '--tls-verify=true',
                                       'docker://' + reference], environment)
        if 'sha256:' + hashlib.sha256(raw_manifest.encode('utf-8')).hexdigest() != digest:
            raise ValueError('remote manifest bytes differ from the published digest')
        verify_manifest(raw_manifest, image_id)
        source.command(['cosign', 'sign', '--yes', '--tlog-upload=false', '--key', str(key),
                        '--annotation', ANNOTATION + '=' + handoff_digest, reference], environment)
        verified = source.command(['cosign', 'verify', '--insecure-ignore-tlog=true',
                                   '--key', str(bundle / 'cosign.pub'), reference], environment)
        verify_signature(verified, reference, handoff_digest)
        return {'schema': 1, 'purpose': 'tilo-retained-development-raw-rootfs',
                'image': reference, 'handoff': receipt, 'handoff_sha256': handoff_digest,
                'hardware_profile': profile, 'published': True, 'installable': False,
                'sealed': False, 'kernel_version': kernel_version,
                'layer_private_material_audit': 'not performed by this helper; required before publication',
                'completion_required': ['rebind public trust and origin', 'regenerate initramfs',
                                        'sign loader', 'seal and sign UKIs', 'disk and VM verification']}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('receipt', 'private-key', 'public-bundle'):
        parser.add_argument('--' + name, required=True, type=Path)
    for name in ('revision', 'architecture', 'manifest-sha256', 'target'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--generation', required=True, type=int)
    parser.add_argument('--source-date-epoch', required=True, type=int)
    try:
        print(json.dumps(retain(parser.parse_args()), indent=2, sort_keys=True))
    except (OSError, ValueError, TypeError, KeyError) as error:
        parser.exit(1, f'raw rootfs retention failed: {type(error).__name__}: {error}\n')


if __name__ == '__main__':
    main()
