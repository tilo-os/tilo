#!/usr/bin/env python3
"""Compute an immutable local image's bootc digest without installing it.

Uses bootc 1.16.4's OCI-storage computation, matching its fresh-install boot
transformation and default EROFS format. Temporary composefs data needs space
under /var/tmp. This receipt is not installation or boot acceptance.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys

ARCHITECTURES = {'aarch64': 'arm64', 'x86_64': 'amd64'}
BOOTC = '/usr/bin/bootc'
VERSION = 'bootc 1.16.4'


def parse_digest(output):
    if not re.fullmatch(r'[0-9a-f]{128}\n?', output) or output.rstrip('\n') == '0' * 128:
        raise ValueError('bootc returned no unambiguous nonzero composefs SHA512')
    return output.rstrip('\n')


def validate_identity(raw, image_id, architecture):
    images = json.loads(raw)
    if not isinstance(images, list) or len(images) != 1 or not isinstance(images[0], dict):
        raise ValueError('expected exactly one local image')
    info = images[0]
    if (info.get('Id', '').removeprefix('sha256:') != image_id.removeprefix('sha256:')
            or info.get('Os') != 'linux' or info.get('Architecture') != ARCHITECTURES[architecture]):
        raise ValueError('local image identity or native architecture differs')


def command(arguments):
    result = subprocess.run(arguments, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        env={'PATH': '/usr/bin:/bin', 'HOME': '/root', 'LANG': 'C.UTF-8'}, timeout=1800)
    if result.returncode:
        raise ValueError(f'{Path(arguments[0]).name} failed (exit {result.returncode})')
    return result.stdout.decode('utf-8', 'strict')


def host_inputs(image_id, architecture):
    if not re.fullmatch(r'sha256:[0-9a-f]{64}', image_id):
        raise ValueError('full immutable local image ID required')
    if (architecture not in ARCHITECTURES or sys.platform != 'linux'
            or platform.machine() != architecture or os.getuid() != 0 or os.geteuid() != 0
            or Path('/proc/self/uid_map').read_text().split() != ['0', '0', '4294967295']):
        raise ValueError('already-root native Linux outside user namespaces required')
    # Both tools address the same default rootful store, never a caller-selected
    # remote engine or an accidentally different rootless/custom store.
    for key in ('CONTAINER_HOST', 'CONTAINER_CONNECTION', 'CONTAINER_SSHKEY', 'DOCKER_HOST',
                'CONTAINERS_STORAGE_CONF', 'CONTAINERS_CONF', 'CONTAINERS_CONF_OVERRIDE',
                'STORAGE_DRIVER', 'STORAGE_OPTS'):
        if os.environ.get(key):
            raise ValueError('remote or custom container storage configuration unsupported')


def compute(image_id, architecture):
    host_inputs(image_id, architecture)
    version = command([BOOTC, '--version']).strip()
    if version != VERSION:
        raise ValueError('trusted host bootc must be exactly version 1.16.4')
    validate_identity(command(['/usr/bin/podman', '--remote=false', 'image', 'inspect', image_id]),
                      image_id, architecture)
    with open(BOOTC, 'rb') as stream:
        tool_digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    digest = parse_digest(command([BOOTC, 'container', 'compute-composefs-digest-from-storage', image_id]))
    return {'schema': 1, 'purpose': 'development-composefs-storage-digest', 'image_id': image_id,
            'architecture': architecture, 'bootc_version': version, 'bootc_sha256': tool_digest,
            'composefs_digest': digest, 'repository_format': 'bootc-1.16.4-fresh-install-default',
            'installation_verified': False, 'boot_verified': False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image-id', required=True)
    parser.add_argument('--architecture', required=True, choices=tuple(ARCHITECTURES))
    args = parser.parse_args()
    try:
        print(json.dumps(compute(args.image_id, args.architecture), sort_keys=True))
    except (ValueError, OSError, TypeError, subprocess.SubprocessError) as error:
        parser.exit(1, f'composefs digest: {error}\n')


if __name__ == '__main__':
    main()
