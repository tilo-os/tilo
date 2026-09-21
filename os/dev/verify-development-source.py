#!/usr/bin/env python3
"""Verify a signed native development OCI image without executing image code.

Run inside `podman unshare` for rootless storage, or as root for rootful storage.
The JSON result is an audit receipt, not an authorization token: installation
must repeat verification and enforce the image's container signature policy.
"""
import argparse
from contextlib import contextmanager
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import tempfile
from typing import NamedTuple

import hardware_profile as hardware

spec = importlib.util.spec_from_file_location('development_trust', Path(__file__).with_name('development-trust.py'))
trust = importlib.util.module_from_spec(spec)
spec.loader.exec_module(trust)
ARCHITECTURES = {'x86_64': 'amd64', 'aarch64': 'arm64'}


class VerifiedSource(NamedTuple):
    image_id: str
    root: Path
    public_bundle: Path
    receipt: dict
    # Context-lifetime private path only; never include it in public receipts.
    auth_file: Path | None = None


def snapshot_auth(original, destination):
    """Bounded private snapshot shared by verification and its active consumer."""
    descriptor = os.open(original, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as handle:
        info = os.fstat(handle.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > 1024 * 1024:
            raise ValueError('registry auth must be a bounded regular file')
        contents = handle.read(1024 * 1024 + 1)
    if not contents or len(contents) > 1024 * 1024:
        raise ValueError('registry auth must be populated and bounded')
    with destination.open('xb') as handle:
        os.fchmod(handle.fileno(), 0o600)
        handle.write(contents)


def inputs(source, target, architecture, revision):
    selected = hardware.development_target(target, architecture)
    if not re.fullmatch(re.escape(selected['repository']) + r'@sha256:[0-9a-f]{64}', source):
        raise ValueError('source must be an immutable native image in the bound development repository')
    if architecture not in ARCHITECTURES or not re.fullmatch(r'[0-9a-f]{40}', revision):
        raise ValueError('expected native architecture and full Git revision are required')
    return selected


def signature_payloads(raw, source):
    if len(raw) > 1024 * 1024:
        raise ValueError('signature verification output is too large')
    payloads = json.loads(raw, object_pairs_hook=trust.unique_object)
    repository, digest = source.split('@')
    if not isinstance(payloads, list) or not 1 <= len(payloads) <= 32:
        raise ValueError('signature verifier returned no bounded payload list')
    for payload in payloads:
        if not isinstance(payload, dict) or not isinstance(payload.get('critical'), dict):
            raise ValueError('invalid signature payload')
        critical = payload.get('critical', {})
        if (not isinstance(critical.get('image'), dict) or not isinstance(critical.get('identity'), dict)
                or critical.get('type') != 'cosign container image signature'
                or critical.get('image', {}).get('docker-manifest-digest') != digest
                or critical.get('identity', {}).get('docker-reference') != repository):
            raise ValueError('verified signature does not bind the exact source repository and digest')


def image_identity(raw, source, architecture, revision):
    images = json.loads(raw, object_pairs_hook=trust.unique_object)
    if not isinstance(images, list) or len(images) != 1:
        raise ValueError('expected exactly one native image')
    info = images[0]
    if (info.get('Os') != 'linux' or info.get('Architecture') != ARCHITECTURES[architecture]
            or info.get('Digest') != source.split('@')[1]
            or source not in info.get('RepoDigests', [])
            or (info.get('Labels') or {}).get('org.opencontainers.image.revision') != revision
            or not re.fullmatch(r'(?:sha256:)?[0-9a-f]{64}', info.get('Id', ''))):
        raise ValueError('native image digest, architecture, source revision or identity differs')
    return info['Id']


def oci_evidence(raw, source):
    images = json.loads(raw, object_pairs_hook=trust.unique_object)
    if not isinstance(images, list) or len(images) != 1:
        raise ValueError('expected exactly one native image')
    info = images[0]
    layers = (info.get('RootFS') or {}).get('Layers')
    if (not isinstance(layers, list) or not layers
            or any(not isinstance(layer, str)
                   or not re.fullmatch(r'sha256:[0-9a-f]{64}', layer)
                   for layer in layers)):
        raise ValueError('native image lacks exact OCI layer identities')
    config = 'sha256:' + info['Id'].removeprefix('sha256:')
    return {'manifest_digest': source.split('@')[1], 'config_digest': config,
            'layer_digests': layers}


def mounted_identity(root, target, manifest_digest, architecture):
    pending = root / hardware.DEVELOPMENT_PATH / 'initramfs-rebind-required.json'
    if pending.exists() or pending.is_symlink():
        raise ValueError('development trust replacement requires verified initramfs regeneration')
    if hardware.development_origin(root) != target:
        raise ValueError('image development update origin differs')
    manifest = hardware.trusted_path(root, hardware.DEVELOPMENT_PATH / 'manifest.json', regular=True)
    if hashlib.sha256(manifest.read_bytes()).hexdigest() != manifest_digest:
        raise ValueError('image public trust differs from independent public bundle')
    profile = hardware.read_profile(root)
    if profile is not None and (profile['architecture'] != architecture or profile['target_update_ref'] != target):
        raise ValueError('hardware profile differs from selected architecture or origin')
    return profile['id'] if profile else None


def command(argv, env):
    # Tool errors can include registry authentication details. Keep stderr private.
    if argv[0] == 'podman':
        argv = [argv[0], '--remote=false', *argv[1:]]
    result = subprocess.run(argv, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if result.returncode:
        raise ValueError(f'{argv[0]} {argv[1]} failed (exit {result.returncode})')
    return result.stdout.decode('utf-8')


@contextmanager
def verified_source(args):
    """Hold the verified image mount and pinned public snapshot for a consumer.

    Consumers must use image_id, never resolve the mutable update tag again.
    This authenticates source identity only; it does not validate boot artifacts,
    native execution eligibility, generation, a disk, or firmware acceptance.
    """
    selected = inputs(args.source, args.target, args.architecture, args.revision)
    if os.getuid() != 0:
        raise ValueError('run in podman unshare, or as root, to inspect mounted image ownership')
    if os.environ.get('COSIGN_REPOSITORY') or os.environ.get('CONTAINER_HOST') or os.environ.get('CONTAINER_CONNECTION'):
        raise ValueError('signature repository redirects and remote Podman connections are unsupported')
    env = os.environ.copy()
    trust.validate(args.public_bundle, args.manifest_sha256)
    with tempfile.TemporaryDirectory(prefix='tilo-source-verification-') as temporary:
        scratch = Path(temporary)
        bundle = scratch / 'public'
        shutil.copytree(args.public_bundle, bundle, symlinks=True)
        trust.validate(bundle, args.manifest_sha256)
        # Cosign and Podman must use the same explicitly supplied registry auth.
        auth = None
        if env.get('REGISTRY_AUTH_FILE'):
            auth = scratch / 'config.json'
            snapshot_auth(env['REGISTRY_AUTH_FILE'], auth)
            env['DOCKER_CONFIG'] = str(scratch)
            env['REGISTRY_AUTH_FILE'] = str(auth)
        verify_command = ['cosign', 'verify', '--key', str(bundle / 'cosign.pub')]
        if not selected['tls_required']:
            verify_command += ['--allow-http-registry']
        if args.offline_key_signature:
            verify_command += ['--insecure-ignore-tlog=true']
        verify_command += [args.source]
        signature_raw = command(verify_command, env)
        signature_payloads(signature_raw, args.source)
        command(['podman', 'pull', '--platform', 'linux/' + ARCHITECTURES[args.architecture],
                 '--tls-verify=' + str(selected['tls_required']).lower(), args.source], env)
        inspect_raw = command(['podman', 'image', 'inspect', args.source], env)
        image_id = image_identity(inspect_raw, args.source, args.architecture, args.revision)
        oci = oci_evidence(inspect_raw, args.source)
        root_text = command(['podman', 'image', 'mount', image_id], env).strip()
        try:
            root = Path(root_text)
            if not root.is_absolute() or '\n' in root_text:
                raise ValueError('invalid local image mount result')
            profile = mounted_identity(root, args.target, args.manifest_sha256, args.architecture)
            canonical_id = 'sha256:' + image_id.removeprefix('sha256:')
            receipt = {'schema': 1, 'purpose': 'tilo-development-source-verification',
                       'source': args.source, 'target_update_ref': args.target,
                       'architecture': args.architecture, 'revision': args.revision,
                       'manifest_sha256': args.manifest_sha256, 'hardware_profile': profile,
                       'image_id': canonical_id,
                       'oci': oci,
                       'signature': {
                           'verified_payload_sha256': hashlib.sha256(
                               signature_raw.encode()).hexdigest(),
                           'transparency_log_required': not args.offline_key_signature,
                       },
                       'transparency_log_required': not args.offline_key_signature}
            yield VerifiedSource(canonical_id, root, bundle, receipt, auth)
        finally:
            command(['podman', 'image', 'unmount', image_id], env)


def verify(args):
    with verified_source(args) as verified:
        return verified.receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('source', 'target', 'architecture', 'revision', 'manifest-sha256'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--public-bundle', required=True, type=Path)
    parser.add_argument('--offline-key-signature', action='store_true',
                        help='development only: verify key signature without requiring a transparency log entry')
    args = parser.parse_args()
    try:
        print(json.dumps(verify(args), indent=2, sort_keys=True))
    except (OSError, ValueError, TypeError, KeyError) as error:
        parser.exit(1, f'development source verification: {error}\n')


if __name__ == '__main__':
    main()
