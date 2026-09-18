#!/usr/bin/env python3
"""Acquire the pinned signed Wine SRPM through configured Fedora source repos.

Run in disposable native x86-64 Fedora 44 tooling with dnf5 download and rpmkeys.
Failed attempts retain their temporary directory and logs; output is published
only after complete verification, without replacing an existing destination.
"""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import tempfile

import wine_rpm_contract as contract


def require_host():
    release = dict(line.split('=', 1) for line in Path('/etc/os-release').read_text().splitlines()
                   if '=' in line)
    if (platform.machine() != 'x86_64' or release.get('ID', '').strip('"') != 'fedora'
            or release.get('VERSION_ID', '').strip('"') != '44'):
        raise ValueError('source acquisition requires native x86-64 Fedora 44 tooling')


def publish(stage, destination):
    # POSIX rename can replace an existing empty directory. Linux's explicit
    # NOREPLACE operation closes that race while preserving atomic publication.
    libc = ctypes.CDLL(None, use_errno=True)
    rename = libc.renameat2
    rename.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    rename.restype = ctypes.c_int
    if rename(-100, os.fsencode(stage), -100, os.fsencode(destination), 1):
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error), str(destination))


def acquire(destination, pin_path, key_path):
    require_host()
    pin = json.loads(Path(pin_path).read_text())
    filename = pin['filename']
    if not re.fullmatch(r'wine-[0-9][A-Za-z0-9.+_]*-[A-Za-z0-9.+_]+\.src\.rpm', filename):
        raise ValueError('invalid pinned source filename')
    if filename != f"wine-{pin['version']}-{pin['release']}.src.rpm":
        raise ValueError('source filename does not match pinned version/release')
    destination = Path(destination).absolute()
    if destination.exists() or destination.is_symlink():
        raise FileExistsError(f'output already exists: {destination}')
    if not destination.parent.is_dir():
        raise ValueError('output parent must already exist')
    # Installed Fedora key paths may legitimately be symlinks. Authenticate
    # the bytes first, then stage a regular file for the isolated trust check.
    key = Path(key_path).read_bytes()
    if hashlib.sha256(key).hexdigest() != pin['fedora_key_sha256']:
        raise ValueError('installed Fedora key does not match pinned identity')
    # Non-hidden staging keeps failure evidence visible to CI artifact uploaders.
    stage = Path(tempfile.mkdtemp(prefix=f'{destination.name}.fetch-', dir=destination.parent))
    command = ['dnf5', 'download', '--source', f'--destdir={stage}', filename.removesuffix('.src.rpm')]
    try:
        (stage / 'fedora-44.asc').write_bytes(key)
        with (stage / 'download.log').open('wb') as log:
            subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, check=True,
                           env={**os.environ, 'LC_ALL': 'C'})
        if sorted(p.name for p in stage.glob('*.rpm')) != [filename]:
            raise ValueError('download must contain exactly the pinned source RPM')
        signature = contract.source(stage, pin)
        (stage / 'signature.log').write_text(signature + '\n')
        (stage / 'acquisition.json').write_text(json.dumps({
            'schema': 1, 'source': pin, 'command': command,
            'method': 'configured Fedora source repositories; URLs resolved by DNF',
            'installed_key_path': str(key_path), 'signature_verified': True,
        }, indent=2) + '\n')
        publish(stage, destination)
    except Exception as error:
        (stage / 'failure.log').write_text(f'{type(error).__name__}: {error}\n')
        raise RuntimeError(f'source acquisition failed; evidence retained at {stage}: {error}') from error
    return destination


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('--pin', type=Path, default=Path(__file__).with_name('wine-source.json'))
    parser.add_argument('--key', type=Path,
                        default=Path('/etc/pki/rpm-gpg/RPM-GPG-KEY-fedora-44-primary'))
    args = parser.parse_args()
    print(acquire(args.output, args.pin, args.key))
