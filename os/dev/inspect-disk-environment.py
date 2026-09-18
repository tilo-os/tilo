#!/usr/bin/env python3
"""Bounded, read-only observations from a native disposable CI guest.

Never opens block devices, mounts filesystems, installs packages or writes disks.
Runtime observations do not prove absence of physical-device passthrough; that
requires independent review of the runner provisioner and actual pod spec.
"""
import gzip
import json
import os
from pathlib import Path
import platform
import stat
import subprocess

LIMIT = 4 * 1024 * 1024
OPTIONS = ('CONFIG_BLK_DEV_LOOP', 'CONFIG_BTRFS_FS', 'CONFIG_EXT4_FS',
           'CONFIG_FS_VERITY', 'CONFIG_OVERLAY_FS', 'CONFIG_EROFS_FS',
           'CONFIG_DM_VERITY')


def text(path, limit=8192):
    try:
        with Path(path).open('rb') as stream:
            data = stream.read(limit + 1)
        return data.decode('utf-8', 'replace') if len(data) <= limit else None
    except OSError:
        return None


def version(argv):
    try:
        result = subprocess.run(argv, capture_output=True, timeout=10, text=True,
                                env={'PATH': '/usr/bin:/usr/sbin:/bin', 'LANG': 'C'})
        return {'exit_code': result.returncode, 'stdout': result.stdout[:4096]}
    except (OSError, subprocess.TimeoutExpired):
        return {'exit_code': None, 'stdout': None}


def kernel_config():
    # /boot/config-* belongs to the container image, not necessarily the running
    # Kata kernel. Only the running kernel's own embedded config is accepted.
    values = {option: None for option in OPTIONS}
    try:
        with gzip.open('/proc/config.gz', 'rb') as stream:
            raw = stream.read(LIMIT + 1)
        if len(raw) > LIMIT:
            raise ValueError('configuration exceeds bound')
        lines = raw.decode('ascii').splitlines()
        for option in OPTIONS:
            matches = [line.split('=', 1)[1] for line in lines if line.startswith(option + '=')]
            if f'# {option} is not set' in lines:
                matches.append('n')
            if len(matches) == 1 and matches[0] in ('y', 'm', 'n'):
                values[option] = matches[0]
        return {'source': '/proc/config.gz', 'status': 'read', 'options': values}
    except (OSError, EOFError, UnicodeError, ValueError):
        return {'source': None, 'status': 'unknown', 'options': values}


def observe():
    devices = []
    for path in sorted(Path('/dev').glob('loop*'))[:64]:
        try:
            mode = path.stat().st_mode
            devices.append({'name': path.name, 'character': stat.S_ISCHR(mode),
                            'block': stat.S_ISBLK(mode)})
        except OSError:
            continue
    blocks = []
    for path in sorted(Path('/sys/block').glob('*'))[:64]:
        blocks.append({'name': path.name,
                       'subsystem': str((path / 'device/subsystem').resolve())[:512]})
    config = kernel_config()
    return {'schema': 1, 'purpose': 'tilo-read-only-disk-environment-preflight',
        'architecture': platform.machine(), 'kernel_release': platform.release(),
        'uid': os.getuid(), 'effective_uid': os.geteuid(),
        'uid_map': text('/proc/self/uid_map'),
        'virtual_machine': version(['/usr/bin/systemd-detect-virt', '--vm']),
        'loop_devices': devices, 'block_device_metadata': blocks,
        'tpm_device_names': sorted(path.name for path in Path('/dev').glob('tpm*'))[:32],
        'efi_variables_path_present': Path('/sys/firmware/efi/efivars').exists(),
        'kernel_config': config, 'filesystems': text('/proc/filesystems'),
        'tools': {name: version(argv) for name, argv in {
            'rpm': ['rpm', '--version'], 'bootc': ['bootc', '--version'],
            'systemd': ['rpm', '-q', 'systemd', 'systemd-boot-unsigned', 'systemd-ukify'],
            'measure': ['/usr/lib/systemd/systemd-measure', '--version'],
            'podman': ['podman', '--version'], 'python': ['python3', '--version']}.items()},
        'physical_passthrough_verified_absent': False,
        'disk_operations_performed': False, 'installation_ready': False,
        'limitations': ['Unknown config entries are not successful capability checks.',
                       'Built-in or modular config flags do not prove working filesystem operations.',
                       'Runner provisioning and pod devices require independent review.']}


if __name__ == '__main__':
    receipt = observe()
    Path('/receipt.json').write_text(json.dumps(receipt, indent=2, sort_keys=True) + '\n')
    print('Read-only guest preflight recorded; installation readiness remains unproven.')
