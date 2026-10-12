#!/usr/bin/env python3
"""Observe installed prerequisites, never mint migration readiness."""
import grp
import hashlib
import json
import os
from pathlib import Path
import pwd
import subprocess

TOOLS = (
    '/usr/bin/flatpak', '/usr/bin/ostree', '/usr/bin/bwrap',
    '/usr/bin/dbus-daemon', '/usr/bin/dbus-run-session', '/usr/bin/gdbus',
    '/usr/bin/pkttyagent', '/usr/lib/polkit-1/polkitd',
    '/usr/libexec/udisks2/udisksd', '/usr/lib/systemd/systemd-udevd',
    '/usr/bin/udevadm', '/usr/bin/gnome-keyring-daemon', '/usr/bin/login',
    '/usr/bin/runuser', '/usr/sbin/runuser', '/usr/bin/useradd',
    '/usr/bin/chpasswd', '/usr/bin/systemd-sysusers', '/usr/bin/systemd-tmpfiles',
    '/usr/bin/update-ca-trust', '/usr/bin/openssl', '/usr/bin/qemu-nbd',
    '/usr/bin/sfdisk', '/usr/bin/partx', '/usr/bin/lsblk', '/usr/bin/findmnt',
    '/usr/bin/mkfs.ext4', '/usr/bin/mkfs.btrfs', '/usr/bin/mount',
    '/usr/bin/umount', '/usr/bin/gpg', '/usr/bin/gpgconf',
)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    if os.getuid() != 0:
        raise RuntimeError('image prerequisite observation requires root')
    manifest = json.loads(Path(
        '/usr/share/tilo-native-prerequisites/source-inputs.json').read_text())
    for source, row in manifest['sources'].items():
        path = Path(row['installed'])
        metadata = path.lstat()
        if not path.is_file() or path.is_symlink() or metadata.st_uid != 0 \
                or metadata.st_mode & 0o7777 != row['mode'] or digest(path) != row['sha256']:
            raise RuntimeError(f'installed source prerequisite differs: {source}')
    tools = {}
    for name in TOOLS:
        path = Path(name)
        resolved = path.resolve(strict=True)
        if not resolved.is_file() or resolved.stat().st_uid != 0 \
                or resolved.stat().st_mode & 0o022 or not os.access(path, os.X_OK):
            raise RuntimeError(f'missing or unsafe native prerequisite: {name}')
        tools[name] = {'resolved': str(resolved), 'sha256': digest(resolved)}
    pam = {}
    for name in ('login', 'system-auth', 'password-auth', 'postlogin'):
        path = Path('/etc/pam.d') / name
        resolved = path.resolve(strict=True)
        if not resolved.is_file() or resolved.stat().st_uid != 0 \
                or resolved.stat().st_mode & 0o022 or not resolved.read_bytes():
            raise RuntimeError(f'actual distro PAM prerequisite missing: {name}')
        pam[name] = {'resolved': str(resolved), 'sha256': digest(resolved)}
    for name in ('wheel', 'video', 'tilo-identity'):
        grp.getgrnam(name)
    for name in ('dbus', 'polkitd'):
        pwd.getpwnam(name)
    packages = subprocess.run(
        ['/usr/bin/rpm', '-qa', '--qf', '%{NAME} %{VERSION}-%{RELEASE}.%{ARCH}\n'],
        check=True, capture_output=True, text=True, timeout=30).stdout.splitlines()
    print(json.dumps({'schema': 1, 'qualification': False, 'tools': tools,
                      'pam': pam, 'packages': sorted(packages),
                      'source_inputs_sha256': digest(Path(
                          '/usr/share/tilo-native-prerequisites/source-inputs.json')),
                      'flatpak_runtime_preparer_present': Path(
                          '/usr/libexec/tilo-flatpak-runnable-runtime/prepare-runtime.py').is_file(),
                      'candidate_tools_are_separate_measured_inputs': True},
                     sort_keys=True, indent=2))


if __name__ == '__main__':
    main()
