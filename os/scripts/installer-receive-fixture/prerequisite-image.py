#!/usr/bin/env python3
"""Assemble text-only prerequisites for the owned native receiving fixture.

This is not a Tilo OS image or a qualification receipt. Candidate executables,
signed SDKs and public service assets remain independently admitted inputs.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import stat

ROOT = Path(__file__).resolve().parents[3]
BASE = 'sha256:ce2ba1165f919362e3e617456def5e6c20d936aa7ba418074602c2bc40a92e91'
PACKAGES = (
    'flatpak ostree bubblewrap xdg-dbus-proxy dbus-daemon dbus-tools polkit '
    'udisks2 systemd-udev systemd-pam shadow-utils util-linux util-linux-core '
    'glib2 gnome-keyring gnome-keyring-pam pam authselect ca-certificates '
    'openssl btrfs-progs e2fsprogs dosfstools qemu-img gnupg2 cryptsetup fuse3 '
    'efibootmgr'
).split()
ASSETS = {
    **{f'os/scripts/installer-source-fixture/flatpak-runtime/{name}':
       (f'usr/libexec/tilo-flatpak-runnable-runtime/{name}', mode)
       for name, mode in (
           ('prepare-runtime.py', 0o755),
           ('dependencies/assemble-app-runtime.py', 0o644),
           ('dependencies/bounded_native.py', 0o644),
           ('dependencies/common.py', 0o644),
       )},
    **{f'os/usr/libexec/{name}': (f'usr/libexec/{name}', 0o755) for name in (
        'tilo-firstboot-user', 'tilo-store-volume',
    )},
    'os/etc/pam.d/tilo-unlock': ('etc/pam.d/tilo-unlock', 0o644),
    'os/usr/lib/sysusers.d/tilo-identity.conf':
        ('usr/lib/sysusers.d/tilo-identity.conf', 0o644),
    'os/usr/lib/tmpfiles.d/tilo.conf': ('usr/lib/tmpfiles.d/tilo.conf', 0o644),
    'os/registry-runtime-publisher.hex':
        ('etc/tilo/runtime-bundle-trusted-publishers.txt', 0o644),
    'crates/platform/imager-backend/src/setup/flatpak_transfer/portable/native_tests/runnable/fixture-app':
        ('usr/libexec/tilo-installer-source-fixture/fixture-app', 0o755),
    'os/scripts/installer-receive-fixture/prerequisite-readiness.py':
        ('usr/libexec/tilo-native-prerequisite-readiness', 0o755),
}


def immutable_base(value):
    if not re.fullmatch(
        r'(?:[a-z0-9][a-z0-9./:_-]*@)?sha256:[0-9a-f]{64}', value
    ):
        raise ValueError('prerequisite base must be an immutable image ID or digest reference')
    return value


def assemble(destination, base=BASE):
    base = immutable_base(base)
    destination.mkdir(mode=0o700)
    inputs = {}
    for source, (installed, mode) in ASSETS.items():
        path = ROOT / source
        before = path.lstat()
        if not stat.S_ISREG(before.st_mode) or before.st_size > 2 * 1024 * 1024:
            raise RuntimeError(f'nonregular or oversized prerequisite: {source}')
        data = path.read_bytes()
        after = path.lstat()
        if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns,
            before.st_ctime_ns) != (after.st_dev, after.st_ino, after.st_size,
                                   after.st_mtime_ns, after.st_ctime_ns):
            raise RuntimeError(f'prerequisite changed: {source}')
        digest = hashlib.sha256(data).hexdigest()
        if source.endswith('/fixture-app') and digest != (
            '51102c91a2305b65f8069d8450f93f3ba09cde5cda9dee6521f52b21d9ec9a5a'
        ):
            raise RuntimeError('canonical runnable Flatpak payload differs')
        target = destination / 'rootfs' / installed
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        target.chmod(mode)
        inputs[source] = {'installed': '/' + installed, 'sha256': digest, 'mode': mode}
    manifest = {'schema': 1, 'base': base, 'packages': PACKAGES,
                'sources': inputs, 'qualification': False,
                'candidate_executables_included': False,
                'flatpak_runtime_preparer_included': True}
    provenance = destination / 'rootfs/usr/share/tilo-native-prerequisites/source-inputs.json'
    provenance.parent.mkdir(parents=True)
    provenance.write_text(json.dumps(manifest, sort_keys=True, indent=2) + '\n')
    provenance.chmod(0o644)
    (destination / 'Containerfile').write_text(
        f'FROM {base}\n'
        + 'RUN dnf install -y ' + ' '.join(PACKAGES) + ' && dnf clean all\n'
        + 'COPY rootfs/ /\n'
        + 'RUN systemd-sysusers /usr/lib/sysusers.d/tilo-identity.conf '
        + '&& /usr/libexec/tilo-native-prerequisite-readiness '
        + '> /usr/share/tilo-native-prerequisites/observed-readiness.json\n'
    )
    shutil.copyfile(provenance, destination / 'source-inputs.json')
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True,
                        help='exclusive new build context directory')
    parser.add_argument('--base', type=immutable_base, default=BASE,
                        help='immutable local image ID or registry digest reference')
    args = parser.parse_args()
    assemble(args.output.absolute(), args.base)
    print(str(args.output.absolute()))


if __name__ == '__main__':
    main()
