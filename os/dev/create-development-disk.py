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
import base64
import hashlib
from contextlib import ExitStack, contextmanager
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time

import development_prediction as prediction
import installer_bootc_helper
import installer_target_artifacts
from private_disk_image import PrivateDiskImage, loop_partition_nodes

spec = importlib.util.spec_from_file_location('development_source', Path(__file__).with_name('verify-development-source.py'))
source = importlib.util.module_from_spec(spec)
spec.loader.exec_module(source)
spec = importlib.util.spec_from_file_location('installed_disk_inspector', Path(__file__).with_name('verify-installed-development-disk.py'))
inspector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inspector)

FLAGS = ('--generic-image', '--composefs-backend', '--bootloader',
         '--root-mount-spec', '--source-imgref', '--target-imgref', '--target-transport',
         '--enforce-container-sigpolicy', '--run-fetch-check')

TRANSIENT_CRUN_NAMESPACE_ERROR = (
    b'error: reading /proc/1/ns/ipc: No such file or directory (os error 2)')

DIAGNOSTIC_STREAM_LIMIT = 256 * 1024
ESP = 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b'
TILO_ROOT = '7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab'


@contextmanager
def private_source_store_scratch(parent=Path('/work')):
    info = parent.lstat()
    if (not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_gid != 0
            or stat.S_IMODE(info.st_mode) != 0o700):
        raise ValueError('source store scratch parent is not root-owned and private')
    root = Path(tempfile.mkdtemp(prefix='tilo-source-store-scratch-', dir=parent))
    os.chmod(root, 0o700)
    try:
        yield root
    finally:
        shutil.rmtree(root)
        if root.exists():
            raise ValueError('source store scratch survived cleanup')


def _bounded_stream(data):
    if len(data) <= DIAGNOSTIC_STREAM_LIMIT:
        return data, False
    half = DIAGNOSTIC_STREAM_LIMIT // 2
    return data[:half] + data[-half:], True


def write_failure_diagnostic(path, argv, result):
    stdout, stdout_truncated = _bounded_stream(result.stdout)
    stderr, stderr_truncated = _bounded_stream(result.stderr)
    document = {
        'schema': 1,
        'purpose': 'tilo-supervised-bootc-install-failure',
        'stage': 'bootc-install-to-filesystem',
        'argv_sha256': hashlib.sha256(json.dumps(argv, separators=(',', ':')).encode()).hexdigest(),
        'returncode': result.returncode,
        'stdout_base64': base64.b64encode(stdout).decode(),
        'stderr_base64': base64.b64encode(stderr).decode(),
        'stdout_truncated': stdout_truncated,
        'stderr_truncated': stderr_truncated,
    }
    encoded = (json.dumps(document, sort_keys=True, separators=(',', ':')) + '\n').encode()
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(encoded); stream.flush(); os.fsync(stream.fileno())
    except BaseException:
        try: os.unlink(path)
        except FileNotFoundError: pass
        raise


def command(argv, *, failure_diagnostic=None, input_bytes=None):
    if input_bytes is not None and (not isinstance(input_bytes, bytes) or len(input_bytes) > 4096):
        raise ValueError('command input is invalid or oversized')
    result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            input=input_bytes,
                            env={'PATH': '/usr/bin:/bin', 'HOME': '/root', 'LANG': 'C.UTF-8'})
    if result.returncode:
        if failure_diagnostic is not None:
            write_failure_diagnostic(failure_diagnostic, argv, result)
        # Registry/tool diagnostics may contain credentials; do not echo them.
        raise ValueError(f'{Path(argv[0]).name} failed (exit {result.returncode})')
    return result.stdout.decode('utf-8', 'strict')


def bootc_install(argv, disk, failure_diagnostic):
    """Run once against a prepared filesystem; any failure discards the disk."""
    disk.validate()
    try:
        return command(argv, failure_diagnostic=failure_diagnostic)
    finally:
        disk.validate()


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


def ensure_private_device(path, kind, major, minor):
    expected = os.makedev(major, minor)
    try:
        info = path.lstat()
    except FileNotFoundError:
        os.mknod(path, kind | 0o600, expected)
        info = path.lstat()
    if (stat.S_IFMT(info.st_mode) != kind or stat.S_IMODE(info.st_mode) != 0o600
            or info.st_uid != 0 or info.st_gid != 0 or info.st_rdev != expected):
        raise ValueError(f'{path} is not the expected root-owned private device')


def ensure_loop_devices(devices=Path('/dev'), sys_block=Path('/sys/block'),
                        sys_control=Path('/sys/class/misc/loop-control/dev'),
                        proc_devices=Path('/proc/devices')):
    expected = {f'loop{index}' for index in range(8)}
    observed = {path.name for path in sys_block.glob('loop*')}
    rows = [line.split() for line in proc_devices.read_text().splitlines()]
    if observed != expected or rows.count(['7', 'loop']) != 1:
        raise ValueError('kernel does not expose the expected loop block devices')
    if sys_control.read_text().strip() != '10:237':
        raise ValueError('kernel does not expose the expected loop control device')
    ensure_private_device(devices / 'loop-control', stat.S_IFCHR, 10, 237)
    for index in range(8):
        if (sys_block / f'loop{index}' / 'dev').read_text().strip() != f'7:{index}':
            raise ValueError('kernel loop block device identity differs from expected')
        ensure_private_device(devices / f'loop{index}', stat.S_IFBLK, 7, index)
    available = command(['/usr/sbin/losetup', '--find']).strip()
    if available not in {f'/dev/loop{index}' for index in range(8)}:
        raise ValueError('loop setup did not return an exact provisioned device')


@contextmanager
def prepared_install_filesystem(disk, parent=Path('/work')):
    """Create and mount the exact Tilo GPT layout on a held new disk file."""
    inspector.require_private_namespace()
    disk.validate()
    fd = disk.fileno()
    layout = (
        'label: gpt\n'
        f'size=1GiB, type={ESP}, name="EFI-SYSTEM"\n'
        f'type={TILO_ROOT}, name="root"\n'
    ).encode()
    with tempfile.TemporaryDirectory(prefix='tilo-install-root-', dir=parent) as directory, \
            ExitStack() as cleanup:
        loop = inspector.command(
            ['/usr/sbin/losetup', '--find', '--show', '--partscan', f'/proc/self/fd/{fd}'],
            pass_fds=(fd,)).decode().strip()
        if not re.fullmatch(r'/dev/loop[0-9]+', loop):
            raise ValueError('unexpected allocated install loop identity')
        cleanup.callback(inspector.command, ['/usr/sbin/losetup', '--detach', loop])
        command(['/usr/sbin/sfdisk', '--wipe=always', loop], input_bytes=layout)
        nodes = cleanup.enter_context(loop_partition_nodes(loop))
        table = json.loads(inspector.command(['/usr/sbin/sfdisk', '--json', loop]))
        if inspector.partitions(table, loop) != [str(path) for path in nodes]:
            raise ValueError('prepared partition identity differs')
        esp_device, root_device = map(str, nodes)
        inspector.command(['/usr/sbin/mkfs.vfat', '-n', 'TILO-EFI', esp_device])
        inspector.command(['/usr/sbin/mkfs.btrfs', '-f', '-L', 'Tilo root', root_device])
        root = Path(directory) / 'target'
        root.mkdir(mode=0o700)
        inspector.command(['/usr/bin/mount', '-t', 'btrfs', '-o',
                           'rw,nodev,nosuid', root_device, str(root)])
        cleanup.callback(inspector.command, ['/usr/bin/umount', str(root)])
        boot = root / 'boot'
        boot.mkdir(mode=0o755)
        inspector.command(['/usr/bin/mount', '-t', 'vfat', '-o',
                           'rw,nodev,nosuid,noexec', esp_device, str(boot)])
        cleanup.callback(inspector.command, ['/usr/bin/umount', str(boot)])
        yield root
    disk.validate()


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
    ensure_loop_devices()
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
    helper_binary, helper_artifact = installer_bootc_helper.verify_artifact(
        args.installer_bootc_directory, args.installer_bootc_sha256,
        args.architecture)
    helper_identity = {
        'architecture': helper_artifact['architecture'],
        'sha256': helper_artifact['binary_sha256'],
        'build_id': helper_artifact['binary_build_id'],
    }
    helper_runtime = None
    source_bootc_identity = None
    if media:
        inspector.require_private_namespace()
    with ExitStack() as disks:
        with source.verified_source(args) as verified:
            source_bootc_identity = installer_bootc_helper.binary_identity(
                verified.root / 'usr/bin/bootc', args.architecture)
            if source_bootc_identity['sha256'] == helper_identity['sha256']:
                raise ValueError('installer helper must differ from source bootc')
            auth_args = []
            if verified.auth_file is not None:
                # skopeo honors REGISTRY_AUTH_FILE while bootc's OSTree
                # importer reads the root runtime auth file directly.  Give
                # both consumers the same held, read-only credential snapshot.
                auth_args = ['--volume', f'{verified.auth_file}:/run/ostree/auth.json:ro',
                             '--env', 'REGISTRY_AUTH_FILE=/run/ostree/auth.json']
            boot = prediction.verify_prediction(verified.root, verified.public_bundle,
                args.manifest_sha256, args.architecture, args.generation,
                args.composefs_digest, args.systemd_version)
            if media:
                inspector.source_boot_selection(verified.root, boot, 'installer-media',
                    getattr(args, 'source_live_uki', None), args.architecture, args.generation, args.composefs_digest)
            if not re.fullmatch(r'sha256:[0-9a-f]{64}', verified.image_id):
                raise ValueError('source verifier did not return an immutable local image ID')
            run = ['/usr/bin/podman', '--remote=false', 'run', '--rm', '--pull=never',
                   '--ipc=private', '--volume',
                   f'{helper_binary}:/run/tilo-installer-bootc:ro']
            help_text = bootc_help([*run, '--network=none', '--cap-drop=ALL',
                                    '--entrypoint=/run/tilo-installer-bootc', verified.image_id,
                                    'install', 'to-filesystem', '--help'])
            if any(not re.search(r'(?<![\w-])' + re.escape(flag) + r'(?![\w-])', help_text)
                   for flag in FLAGS):
                raise ValueError('packaged bootc lacks a required installation flag')
            disk = disks.enter_context(PrivateDiskImage(args.output, args.size_bytes))
            disk.validate()
            source_match = re.fullmatch(
                r'code\.podesta\.ai/[a-z0-9_./-]+@(sha256:[0-9a-f]{64})', args.source)
            if source_match is None:
                raise ValueError('bootc warmup requires the expected immutable registry source')
            wrapper = Path(__file__).with_name('bootc-registry-wrapper.py')
            helper_module = Path(__file__).with_name('installer_bootc_helper.py')
            # Partition and mount only the held new file.  The inner container
            # receives that prepared tree, never the guest root or raw image.
            with prepared_install_filesystem(disk) as install_root, \
                    private_source_store_scratch() as store_scratch:
                runtime_receipt = store_scratch / 'installer-bootc-runtime.json'
                runtime_receipt.touch(mode=0o600)
                bootc_install([*run, '--privileged', '--network=host',
                    '--tmpfs', '/run/udev:ro,nosuid,nodev,noexec,mode=0755', *auth_args,
                    '--volume', '/var/lib/containers:/run/tilo-parent-containers:ro',
                    '--volume', f'{store_scratch}:/run/tilo-source-store-scratch:rw',
                    '--volume', f'{install_root}:/target:rw',
                    '--volume', f'{wrapper}:/run/tilo-bootc-registry-wrapper.py:ro',
                    '--volume', f'{helper_module}:/run/installer_bootc_helper.py:ro',
                    '--volume', f'{runtime_receipt}:/run/tilo-installer-bootc-runtime.json:rw',
                    '--entrypoint=/usr/bin/python3', args.source,
                    '/run/tilo-bootc-registry-wrapper.py',
                    '--installer-bootc-sha256', helper_identity['sha256'],
                    '--installer-bootc-architecture', args.architecture,
                    '--installer-bootc-runtime-receipt',
                    '/run/tilo-installer-bootc-runtime.json',
                    'docker://' + args.source, source_match.group(1), '--',
                    '/run/tilo-installer-bootc', 'install', 'to-filesystem',
                    '--root-mount-spec=', '--generic-image',
                    '--composefs-backend', '--bootloader=systemd',
                    '--source-imgref=docker://' + args.source,
                    '--target-imgref=' + args.target,
                    '--target-transport=registry',
                    '--enforce-container-sigpolicy', '--run-fetch-check', '/target'], disk,
                    args.output.with_name('bootc-install.failure.json'))
                helper_runtime = installer_bootc_helper.verify_runtime_receipt(
                    runtime_receipt, helper_identity)
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
            'installer_bootc_helper': {
                'artifact': helper_artifact,
                'source_binary': source_bootc_identity,
                'runtime': helper_runtime,
            },
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
        cleanup.enter_context(loop_partition_nodes(loop))
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
    parser.add_argument('--installer-bootc-directory', type=Path, required=True)
    parser.add_argument('--installer-bootc-sha256', required=True)
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
