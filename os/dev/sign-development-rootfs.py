#!/usr/bin/python3
"""Add an authenticated development loader to an immutable, unsealed local rootfs.

Requires an already-root caller and an externally provisioned signer. Produces a
local image ID only; it neither seals nor publishes an image or creates a disk.
"""
import argparse
from contextlib import contextmanager
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile

import hardware_profile as hardware

HERE = Path(__file__).resolve().parent


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, HERE / filename)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


source = module('development_source', 'verify-development-source.py')
loader = module('development_loader', 'verify-development-loader.py')
signing = module('development_signer', 'development-signer.py')
trust = source.trust
ARCHITECTURES = source.ARCHITECTURES
LOADER_NAMES = {'x86_64': 'systemd-bootx64.efi', 'aarch64': 'systemd-bootaa64.efi'}
LOCAL_STORAGE_ENVIRONMENT = (
    'HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_RUNTIME_DIR',
    'CONTAINERS_STORAGE_CONF', 'CONTAINERS_CONF', 'CONTAINERS_CONF_OVERRIDE',
    'STORAGE_DRIVER', 'STORAGE_OPTS', 'TMPDIR',
)


def inputs(args):
    if (not re.fullmatch(r'0|[1-9][0-9]{0,11}', args.source_date_epoch)
            or int(args.source_date_epoch) > 253402300799):
        raise ValueError('source date epoch must be canonical nonnegative Unix seconds through year 9999')
    if not re.fullmatch(r'sha256:[0-9a-f]{64}', args.rootfs_id):
        raise ValueError('rootfs must be a full immutable local image ID')
    if args.architecture not in ARCHITECTURES or args.architecture != platform.machine():
        raise ValueError('rootfs signing requires native architecture execution')
    if not re.fullmatch(r'[0-9a-f]{40}', args.revision):
        raise ValueError('a full committed source revision is required')
    hardware.development_target(args.target, args.architecture)
    if os.getuid() != 0:
        raise ValueError('development rootfs signing requires an already-root caller')
    if any(os.environ.get(key) for key in ('CONTAINER_HOST', 'CONTAINER_CONNECTION',
                                         'CONTAINER_SSHKEY', 'DOCKER_HOST')):
        raise ValueError('remote container connections are unsupported')
    if '..' in args.signer.parts:
        raise ValueError('external signer path must be canonical')
    signer = signing.trusted(args.signer)
    if not os.access(signer, os.X_OK):
        raise ValueError('external signer must be executable')
    checkout = HERE.parent.parent
    if signer.is_relative_to(checkout):
        raise ValueError('external signer must be provisioned outside the checkout')
    for name in ('RUNNER_TEMP', 'GITHUB_WORKSPACE'):
        if os.environ.get(name) and signer.resolve().is_relative_to(Path(os.environ[name]).resolve()):
            raise ValueError(f'external signer must be provisioned outside {name}')


def command(arguments):
    environment = {'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8', 'HOME': '/root'}
    if arguments[0] == 'podman':
        arguments = ['/usr/bin/podman', '--remote=false', *arguments[1:]]
        # The shell's raw/final builds and this helper must address the same
        # local store, including explicitly configured VFS stores in CI. Never
        # forward these settings (or unrelated caller secrets) to the signer.
        environment.update({name: os.environ[name] for name in LOCAL_STORAGE_ENVIRONMENT
                            if name in os.environ})
    result = subprocess.run(arguments, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            env=environment)
    if result.returncode:
        stderr = result.stderr.decode(errors='replace').strip()
        detail = f': {stderr[-4096:]}' if stderr else ''
        raise ValueError(
            f'{Path(arguments[0]).name} failed (exit {result.returncode}){detail}')
    return result.stdout.decode()


def identity(raw, expected, architecture, revision):
    images = json.loads(raw, object_pairs_hook=trust.unique_object)
    if not isinstance(images, list) or len(images) != 1:
        raise ValueError('expected one local image')
    info = images[0]
    if (info.get('Id', '').removeprefix('sha256:') != expected.removeprefix('sha256:')
            or info.get('Os') != 'linux' or info.get('Architecture') != ARCHITECTURES[architecture]
            or (info.get('Labels') or {}).get('org.opencontainers.image.revision') != revision):
        raise ValueError('local image identity, architecture or revision differs')


@contextmanager
def mounted(image_id):
    location = command(['podman', 'image', 'mount', image_id]).strip()
    try:
        if not Path(location).is_absolute() or '\n' in location:
            raise ValueError('invalid local mount location')
        yield Path(location)
    finally:
        command(['podman', 'image', 'unmount', image_id])


def produce(args):
    inputs(args)
    trust.validate(args.public_bundle, args.manifest_sha256)
    command_identity = lambda image_id: identity(
        command(['podman', 'image', 'inspect', image_id]), image_id, args.architecture, args.revision)
    command_identity(args.rootfs_id)
    with tempfile.TemporaryDirectory(prefix='tilo-development-rootfs-', dir='/var/lib') as temporary:
        work = Path(temporary)
        public = work / 'public'
        shutil.copytree(args.public_bundle, public, symlinks=True)
        metadata = trust.validate(public, args.manifest_sha256)
        certificate = public / 'efi-signing.crt'
        fingerprint = metadata['identities']['efi-signing.crt']
        relative = Path('usr/lib/systemd/boot/efi') / LOADER_NAMES[args.architecture]
        original = work / 'original.efi'
        with mounted(args.rootfs_id) as root:
            profile = source.mounted_identity(root, args.target, args.manifest_sha256, args.architecture)
            # This command must never add /usr content to an already sealed image.
            for path in (root / 'boot/EFI/Linux', root / 'kernel'):
                if path.exists() or path.is_symlink():
                    raise ValueError('input must be the unsealed rootfs stage, without UKIs or split kernel')
            native_loader = hardware.trusted_path(root, relative, regular=True)
            if native_loader.with_suffix('.efi.signed').exists() or native_loader.with_suffix('.efi.signed').is_symlink():
                raise ValueError('input rootfs already has a signed loader companion')
            loader.snapshot(native_loader, original, 8 * 1024 * 1024)
        signed = work / 'signed.efi'
        command([str(args.signer), 'sign-efi', str(original), str(signed)])
        evidence = loader.verify(original, signed, certificate, fingerprint, args.architecture)
        # Only signed public bytes and checked-in verifier tools enter build contexts.
        payload = work / 'payload'
        payload.mkdir()
        loader.snapshot(signed, payload / 'loader.efi', 8 * 1024 * 1024)
        tools = work / 'tools'
        tools.mkdir()
        for name in ('verify-development-loader.py', 'verify-authenticode-envelope.py'):
            shutil.copyfile(HERE / name, tools / name)
        context = work / 'context'
        context.mkdir()
        shutil.copyfile(HERE.parent / 'Containerfile.development-loader', context / 'Containerfile')
        iidfile = work / 'signed-rootfs.iid'
        command(['podman', 'build', '--pull=never', '--network=none', '--no-cache',
                 '--source-date-epoch', args.source_date_epoch, '--rewrite-timestamp',
                 '--iidfile', str(iidfile), '--build-context', f'raw-rootfs=container-image://{args.rootfs_id}',
                 '--build-context', f'signed-loader={payload}', '--build-context', f'loader-tools={tools}',
                 '--build-arg', f'LOADER_NAME={relative.name}', '--build-arg', f'ARCHITECTURE={args.architecture}',
                 '--build-arg', f'CERTIFICATE_SHA256={fingerprint}',
                 '--build-arg', f'ORIGINAL_SHA256={evidence["original_sha256"]}', str(context)])
        signed_id = iidfile.read_text().strip()
        if not re.fullmatch(r'sha256:[0-9a-f]{64}', signed_id):
            raise ValueError('build did not return a full immutable image ID')
        command_identity(signed_id)
        with mounted(signed_id) as root:
            if source.mounted_identity(root, args.target, args.manifest_sha256, args.architecture) != profile:
                raise ValueError('signed rootfs hardware profile differs')
            installed_original = hardware.trusted_path(root, relative, regular=True)
            installed_signed = hardware.trusted_path(root, Path(str(relative) + '.signed'), regular=True)
            if installed_original.read_bytes() != original.read_bytes():
                raise ValueError('loader layer changed original rootfs loader')
            final_evidence = loader.verify(installed_original, installed_signed, certificate,
                                           fingerprint, args.architecture)
            if final_evidence != evidence:
                raise ValueError('installed signed loader differs from verified signing output')
    return {'schema': 1, 'purpose': 'tilo-development-unsealed-signed-rootfs',
            'rootfs_id': args.rootfs_id, 'signed_rootfs_id': signed_id,
            'architecture': args.architecture, 'revision': args.revision,
            'source_date_epoch': int(args.source_date_epoch),
            'manifest_sha256': args.manifest_sha256, 'target_update_ref': args.target,
            'hardware_profile': profile, 'loader': evidence}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('rootfs-id', 'architecture', 'revision', 'manifest-sha256', 'target', 'source-date-epoch'):
        parser.add_argument('--' + name, required=True)
    for name in ('public-bundle', 'signer'):
        parser.add_argument('--' + name, required=True, type=Path)
    try:
        print(json.dumps(produce(parser.parse_args()), sort_keys=True))
    except (OSError, ValueError, TypeError, KeyError) as error:
        parser.exit(1, f'development rootfs signing: {error}\n')


if __name__ == '__main__':
    main()
