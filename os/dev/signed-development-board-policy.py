#!/usr/bin/env python3
"""Require the existing OS signer for the opt-in development board repository."""
import json
from pathlib import Path
import sys

# These generic VM additions are not maintenance routes for a physical board.
# Remove only these image-owned files, never persisted device credentials/data.
VM_ONLY_PATHS = (
    'usr/libexec/tilo-device-admin', 'usr/libexec/tilo-vmd',
    'usr/lib/systemd/system/tilo-device-admin.service',
    'etc/systemd/system/multi-user.target.wants/tilo-device-admin.service',
    'usr/lib/systemd/system/tilo-vm-restore.service',
    'etc/systemd/system/multi-user.target.wants/tilo-vm-restore.service',
    'usr/lib/systemd/system/tilo-vm-reconcile-image.service',
    'etc/systemd/system/sysinit.target.wants/tilo-vm-reconcile-image.service',
    'usr/lib/systemd/system/tilo-vm-session@.service',
    'usr/lib/udev/rules.d/72-vkms-seat.rules', 'usr/lib/udev/rules.d/73-uinput-seat.rules',
    'usr/lib/modprobe.d/tilo-vm-vkms.conf', 'usr/lib/modules-load.d/tilo-vm.conf',
    'usr/lib/tmpfiles.d/tilo-vm.conf',
)
REPOSITORY = 'registry.tilo.so/tilo/tilo-vm'
REQUIREMENT = [{'type': 'sigstoreSigned', 'keyPath': '/usr/share/tilo/cosign.pub',
                'signedIdentity': {'type': 'matchRepository'}}]


def strengthen(root: Path) -> None:
    if (root / 'usr/lib/tilo/image-flavour').read_text().strip() != 'dev':
        raise ValueError('signed board policy requires the existing development flavour')
    key = root / 'usr/share/tilo/cosign.pub'
    if key.is_symlink() or not key.is_file():
        raise ValueError('existing OS public key is missing or redirected')
    path = root / 'etc/containers/policy.json'
    if path.is_symlink() or not path.is_file():
        raise ValueError('development policy is missing or redirected')
    policy = json.loads(path.read_text())
    docker = policy['transports']['docker']
    if docker.get(REPOSITORY) != [{'type': 'insecureAcceptAnything'}]:
        raise ValueError('expected the ordinary unsigned development scope')
    for name in VM_ONLY_PATHS:
        item = root / name
        for parent in item.relative_to(root).parents:
            if (root / parent).is_symlink():
                raise ValueError('VM route parent is redirected')
        if item.is_dir():
            raise ValueError('VM route file unexpectedly names a directory')
    # A rejected later route must not remove earlier services from the image.
    for name in VM_ONLY_PATHS:
        (root / name).unlink(missing_ok=True)
    docker[REPOSITORY] = REQUIREMENT
    path.write_text(json.dumps(policy, indent=2) + '\n')


def maintenance(root: Path) -> None:
    """Derive the board maintenance flavour from production component bytes."""
    flavour = root / 'usr/lib/tilo/image-flavour'
    if flavour.exists() or flavour.is_symlink():
        raise ValueError('maintenance board requires a production-flavour base')
    if any((root/name).exists() or (root/name).is_symlink() for name in VM_ONLY_PATHS):
        raise ValueError('production maintenance base contains VM access routes')
    key = root / 'usr/share/tilo/cosign.pub'
    path = root / 'etc/containers/policy.json'
    if key.is_symlink() or not key.is_file() or path.is_symlink() or not path.is_file():
        raise ValueError('production key/policy is missing or redirected')
    policy = json.loads(path.read_text())
    docker = policy['transports']['docker']
    if docker.get(REPOSITORY) != [{'type': 'reject'}]:
        raise ValueError('production base must reject the development repository')
    docker[REPOSITORY] = REQUIREMENT
    path.write_text(json.dumps(policy, indent=2) + '\n')
    flavour.write_text('dev\n')


if __name__ == '__main__':
    if sys.argv[1:] and sys.argv[1] == '--maintenance':
        maintenance(Path(sys.argv[2]))
    else:
        strengthen(Path(sys.argv[1]))
