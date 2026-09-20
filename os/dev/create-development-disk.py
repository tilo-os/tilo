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
import stat
import subprocess
import sys
import tempfile
import time

import development_prediction as prediction
import installer_target_artifacts
from private_disk_image import PrivateDiskImage

spec = importlib.util.spec_from_file_location('development_source', Path(__file__).with_name('verify-development-source.py'))
source = importlib.util.module_from_spec(spec)
spec.loader.exec_module(source)
spec = importlib.util.spec_from_file_location('installed_disk_inspector', Path(__file__).with_name('verify-installed-development-disk.py'))
inspector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inspector)

FLAGS = ('--via-loopback', '--generic-image', '--composefs-backend', '--bootloader',
         '--filesystem', '--source-imgref', '--target-imgref',
         '--enforce-container-sigpolicy', '--run-fetch-check')

TRANSIENT_CRUN_NAMESPACE_ERROR = (
    b'error: reading /proc/1/ns/ipc: No such file or directory (os error 2)')


def command(argv):
    result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            env={'PATH': '/usr/bin:/bin', 'HOME': '/root', 'LANG': 'C.UTF-8'})
    if result.returncode:
        # Registry/tool diagnostics may contain credentials; do not echo them.
        raise ValueError(f'{Path(argv[0]).name} failed (exit {result.returncode})')
    return result.stdout.decode('utf-8', 'strict')


def bootc_help(argv, attempts=3, delay_seconds=2):
    """Run the read-only compatibility probe through a narrow crun retry."""
    for attempt in range(attempts):
        result = subprocess.run(
            argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env={'PATH': '/usr/bin:/bin', 'HOME': '/root', 'LANG': 'C.UTF-8'})
        if not result.returncode:
            return result.stdout.decode('utf-8', 'strict')
        if (result.stderr.strip() == TRANSIENT_CRUN_NAMESPACE_ERROR and
                attempt + 1 < attempts):
            time.sleep(delay_seconds)
            continue
        # Do not expose tool diagnostics, even though this probe has no auth.
        raise ValueError(f'{Path(argv[0]).name} failed (exit {result.returncode})')
    raise AssertionError('unreachable')


def ensure_fuse_device(device=Path('/dev/fuse'), misc=Path('/proc/misc'),
                       sys_device=Path('/sys/class/misc/fuse/dev')):
    expected = os.makedev(10, 229)
    try:
        info = device.lstat()
    except FileNotFoundError:
        entries = [line.split() for line in misc.read_text().splitlines()]
        if entries.count(['229', 'fuse']) != 1 or sys_device.read_text().strip() != '10:229':
            raise ValueError('kernel does not expose the expected FUSE misc device')
        os.mknod(device, stat.S_IFCHR | 0o600, expected)
        info = device.lstat()
    if not stat.S_ISCHR(info.st_mode) or info.st_rdev != expected:
        raise ValueError('FUSE device identity differs from character device 10:229')


def reject_firmware_access(mountinfo=Path('/proc/self/mountinfo'),
                           efivars=Path('/sys/firmware/efi/efivars'),
                           devices=Path('/dev'), tpm_class=Path('/sys/class/tpm')):
    # EFI-enabled kernels may expose an empty directory without a variable
    # store. Check actual exposure, including efivarfs mounted elsewhere.
    mounts = mountinfo.read_text().splitlines()
    if not mounts:
        raise ValueError('cannot inspect guest firmware mounts')
    for line in mounts:
        fields = line.split(' - ')
        if len(fields) != 2 or len(fields[1].split()) < 3:
            raise ValueError('cannot inspect guest firmware mounts')
        if fields[1].split()[0] == 'efivarfs':
            raise ValueError('disk creation guest must not expose EFI variables')
    if (efivars.exists() and any(efivars.iterdir())):
        raise ValueError('disk creation guest must not expose EFI variables')
    if any(devices.glob('tpm*')) or (tpm_class.exists() and any(tpm_class.iterdir())):
        raise ValueError('disk creation guest must not expose a TPM')


def guest_inputs(args, *, supervised=False):
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
    if not supervised and command(['/usr/bin/systemd-detect-virt', '--vm']).strip() not in ('kvm', 'qemu'):
        raise ValueError('a disposable QEMU/KVM guest is required')
    # Privileged image execution must not receive a TPM or firmware variable
    # store, even belonging to the temporary build guest.
    reject_firmware_access()
    ensure_fuse_device()
    if ':' in str(args.output):
        raise ValueError('disk output path cannot contain volume option delimiters')
    source.inputs(args.source, args.target, args.architecture, args.revision)
    prediction.boot.expected_inputs(args.architecture, args.generation, args.composefs_digest)


def create(args):
    guest_inputs(args)
    return _create_verified_disk(args)


def create_from_supervised_controller(args, observation):
    """Controller-only API for guests without firmware virtualization markers.

    The authenticated controller must inspect the live Pod, sandbox, effective
    runtime, QEMU devices/fds and exact shared mounts immediately before exec.
    Its observation is an audit record, NOT an authorization token: a guest can
    manufacture JSON. Authorization comes from the controller's inspection and
    authenticated exec, and this API must never be exposed as a CLI option.
    The standalone CLI retains its independent virtualization check.
    """
    # Select only non-secret identity fields; never copy arbitrary controller
    # diagnostics, runtime environments or credentials into the disk receipt.
    fields = ('node_uid', 'pod_uid', 'sandbox_id', 'container_id', 'kernel_sha256')
    if not isinstance(observation, dict) or set(observation) != set(fields):
        raise ValueError('controller observation must contain exact guest identity fields')
    for field in fields:
        pattern = r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}' if field.endswith('_uid') else r'[0-9a-f]{64}'
        if not isinstance(observation[field], str) or not re.fullmatch(pattern, observation[field]):
            raise ValueError('invalid controller observation identity')
    guest_inputs(args, supervised=True)
    receipt = _create_verified_disk(args)
    receipt['controller_observation'] = dict(observation)
    return receipt


def _create_verified_disk(args):
    media = getattr(args, 'boot_profile', 'installed-target') == 'installer-media'
    if getattr(args, 'boot_profile', 'installed-target') not in ('installed-target', 'installer-media'):
        raise ValueError('unsupported disk boot profile')
    media_receipt = None
    if media:
        inspector.require_private_namespace()
    with ExitStack() as disks:
        with source.verified_source(args) as verified:
            auth_args = []
            if verified.auth_file is not None:
                auth_args = ['--volume', f'{verified.auth_file}:/run/tilo-registry-auth.json:ro',
                             '--env', 'REGISTRY_AUTH_FILE=/run/tilo-registry-auth.json']
            boot = prediction.verify_prediction(verified.root, verified.public_bundle,
                args.manifest_sha256, args.architecture, args.generation,
                args.composefs_digest, args.systemd_version)
            if media:
                inspector.source_boot_selection(verified.root, boot, 'installer-media',
                    getattr(args, 'source_live_uki', None), args.architecture, args.generation, args.composefs_digest)
            if not re.fullmatch(r'sha256:[0-9a-f]{64}', verified.image_id):
                raise ValueError('source verifier did not return an immutable local image ID')
            run = ['/usr/bin/podman', '--remote=false', 'run', '--rm', '--pull=never',
                   '--ipc=private']
            help_text = bootc_help([*run, '--network=none', '--cap-drop=ALL',
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
            if media:
                media_receipt = materialize_created_media(disk, verified, args)
        # Source cleanup must succeed before the disk may survive this call.
        file_identity = disk.file_identity()
        disk.retain()
    return {'schema': 1, 'purpose': 'tilo-development-unaccepted-test-disk',
            'source': verified.receipt, 'boot': boot, 'path': str(args.output),
            'file_identity': file_identity,
            'boot_profile': getattr(args, 'boot_profile', 'installed-target'),
            'target_materialization': media_receipt,
            'size_bytes': args.size_bytes, 'disk_created': True,
            'installed_bytes_verified': False, 'vm_boot_verified': False, 'installable': False}


def materialize_created_media(disk, verified, args):
    """Only the creator's held new regular file receives these ESP writes."""
    inspector.require_private_namespace()
    disk.validate()
    fd = disk.fileno()
    with tempfile.TemporaryDirectory(prefix='tilo-new-media-esp-') as directory, ExitStack() as cleanup:
        loop = inspector.command(['/usr/sbin/losetup', '--find', '--show', '--partscan',
                                  f'/proc/self/fd/{fd}'], pass_fds=(fd,)).decode().strip()
        if not re.fullmatch('/dev/loop[0-9]+', loop):
            raise ValueError('unexpected allocated media loop identity')
        cleanup.callback(inspector.command, ['/usr/sbin/losetup', '--detach', loop])
        table = json.loads(inspector.command(['/usr/sbin/sfdisk', '--json', loop]))
        esp_device, _ = inspector.partitions(table, loop)
        esp = Path(directory) / 'esp'
        esp.mkdir()
        inspector.command(['/usr/bin/mount', '-t', 'vfat', '-o', 'rw,nodev,nosuid,noexec', esp_device, str(esp)])
        cleanup.callback(inspector.command, ['/usr/bin/umount', str(esp)])
        receipt = installer_target_artifacts.materialize(verified.root, esp, verified.public_bundle,
            args.manifest_sha256, args.architecture, args.generation, args.composefs_digest, args.systemd_version)
    disk.validate()
    os.fsync(fd)
    return receipt


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
    parser.add_argument('--boot-profile', choices=('installed-target', 'installer-media'), default='installed-target')
    parser.add_argument('--source-live-uki', help='installer-media only: authenticated source boot/EFI/Linux/*.efi')
    try:
        print(json.dumps(create(parser.parse_args()), sort_keys=True))
    except (ValueError, OSError, TypeError, KeyError) as error:
        parser.exit(1, f'development disk creation: {error}\n')


if __name__ == '__main__':
    main()
