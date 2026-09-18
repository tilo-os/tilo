#!/usr/bin/env python3
"""Create an unaccepted test disk inside an explicitly disposable native VM.

No physical installation, publication, seal repair or VM acceptance. Successful
bootc execution is recorded as creation only; installed-byte inspection and boot
acceptance remain mandatory before the candidate can be called installable.

Run only in an operator-provisioned disposable CI guest with no physical-device
passthrough. VM detection and acknowledgement are prerequisites, not proof of
disposability or device isolation. Privileged Podman can access guest devices;
the runner must enforce the no-passthrough boundary outside this program.
"""
import argparse
from contextlib import ExitStack
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys

import development_prediction as prediction
from private_disk_image import PrivateDiskImage

spec = importlib.util.spec_from_file_location('development_source', Path(__file__).with_name('verify-development-source.py'))
source = importlib.util.module_from_spec(spec)
spec.loader.exec_module(source)

FLAGS = ('--via-loopback', '--generic-image', '--composefs-backend', '--bootloader',
         '--filesystem', '--source-imgref', '--target-imgref',
         '--enforce-container-sigpolicy', '--run-fetch-check')



def command(argv):
    result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            env={'PATH': '/usr/bin:/bin', 'HOME': '/root', 'LANG': 'C.UTF-8'})
    if result.returncode:
        # Registry/tool diagnostics may contain credentials; do not echo them.
        raise ValueError(f'{Path(argv[0]).name} failed (exit {result.returncode})')
    return result.stdout.decode('utf-8', 'strict')


def guest_inputs(args):
    if not args.disposable_guest:
        raise ValueError('explicit disposable guest acknowledgement is required')
    if sys.platform != 'linux' or os.getuid() != 0 or os.geteuid() != 0:
        raise ValueError('disk creation requires already-root Linux')
    if args.architecture != platform.machine():
        raise ValueError('disk creation requires native architecture execution')
    if Path('/proc/self/uid_map').read_text().split() != ['0', '0', '4294967295']:
        raise ValueError('user namespaces are not supported for disk creation')
    if any(os.environ.get(name) for name in ('CONTAINER_HOST', 'CONTAINER_CONNECTION',
                                            'CONTAINER_SSHKEY', 'DOCKER_HOST')):
        raise ValueError('remote container configuration is unsupported')
    if command(['/usr/bin/systemd-detect-virt', '--vm']).strip() not in ('kvm', 'qemu'):
        raise ValueError('a disposable QEMU/KVM guest is required')
    # Privileged image execution must not receive a TPM or firmware variable
    # store, even belonging to the temporary build guest.
    if (Path('/sys/firmware/efi/efivars').exists()
            or any(Path('/dev').glob('tpm*'))):
        raise ValueError('disk creation guest must not expose TPM or EFI variables')
    if ':' in str(args.output):
        raise ValueError('disk output path cannot contain volume option delimiters')
    source.inputs(args.source, args.target, args.architecture, args.revision)
    prediction.boot.expected_inputs(args.architecture, args.generation, args.composefs_digest)


def create(args):
    guest_inputs(args)
    with ExitStack() as disks:
        with source.verified_source(args) as verified:
            auth_args = []
            if verified.auth_file is not None:
                auth_args = ['--volume', f'{verified.auth_file}:/run/tilo-registry-auth.json:ro',
                             '--env', 'REGISTRY_AUTH_FILE=/run/tilo-registry-auth.json']
            boot = prediction.verify_prediction(verified.root, verified.public_bundle,
                args.manifest_sha256, args.architecture, args.generation,
                args.composefs_digest, args.systemd_version)
            if not re.fullmatch(r'sha256:[0-9a-f]{64}', verified.image_id):
                raise ValueError('source verifier did not return an immutable local image ID')
            run = ['/usr/bin/podman', '--remote=false', 'run', '--rm', '--pull=never']
            help_text = command([*run, '--network=none', '--cap-drop=ALL',
                                 '--entrypoint=/usr/bin/bootc', verified.image_id,
                                 'install', 'to-disk', '--help'])
            if any(not re.search(r'(?<![\w-])' + re.escape(flag) + r'(?![\w-])', help_text)
                   for flag in FLAGS):
                raise ValueError('packaged bootc lacks a required installation flag')
            disk = disks.enter_context(PrivateDiskImage(args.output, args.size_bytes))
            path = disk.validate()
            # Only a newly allocated file is bound, never the guest root. The
            # operator must provision this VM without device passthrough;
            # privileged Podman still has access to the VM's own devices.
            command([*run, '--privileged', '--network=host', *auth_args,
                '--volume', f'{path}:/output/disk.raw:rw',
                '--entrypoint=/usr/bin/bootc', verified.image_id,
                'install', 'to-disk', '--via-loopback', '--generic-image',
                '--composefs-backend', '--bootloader=systemd', '--filesystem=btrfs',
                '--source-imgref=docker://' + args.source,
                '--target-imgref=' + args.target,
                '--enforce-container-sigpolicy', '--run-fetch-check', '/output/disk.raw'])
            disk.validate()
        # Source cleanup must succeed before the disk may survive this call.
        disk.retain()
    return {'schema': 1, 'purpose': 'tilo-development-unaccepted-test-disk',
            'source': verified.receipt, 'boot': boot, 'path': str(args.output),
            'size_bytes': args.size_bytes, 'disk_created': True,
            'installed_bytes_verified': False, 'vm_boot_verified': False, 'installable': False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('source', 'target', 'architecture', 'revision', 'manifest-sha256',
                 'composefs-digest', 'systemd-version'):
        parser.add_argument('--' + name, required=True)
    for name in ('public-bundle', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ('generation', 'size-bytes'):
        parser.add_argument('--' + name, type=int, required=True)
    parser.add_argument('--offline-key-signature', action='store_true')
    parser.add_argument('--disposable-guest', action='store_true')
    try:
        print(json.dumps(create(parser.parse_args()), sort_keys=True))
    except (ValueError, OSError, TypeError, KeyError) as error:
        parser.exit(1, f'development disk creation: {error}\n')


if __name__ == '__main__':
    main()
