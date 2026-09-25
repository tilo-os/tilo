#!/usr/bin/env python3
"""Opt-in VM cache for the expensive bootc-to-Podman export.

The installed image always takes the normal export path unless a root-owned
runtime marker exists *and* systemd identifies a virtual machine.  The receipt
is only a retry acceleration for one booted image and one exact Podman image;
it is not an alternative source of installation trust.
"""

import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile


MARKER = Path('/run/tilo-installer/vm-bootc-copy-cache.enabled')
RECEIPT_DIR = Path('/var/lib/tilo-installer/vm-copy-cache')
RECEIPT = RECEIPT_DIR / 'vm-bootc-copy-cache-v1.json'
CMDLINE = Path('/proc/cmdline')
IMAGE = 'localhost/bootc'
SHA256 = re.compile(r'sha256:[0-9a-f]{64}\Z')
PODMAN_IMAGE_ID = re.compile(r'(?:sha256:)?[0-9a-f]{64}\Z')
COMPOSEFS = re.compile(r'[0-9a-f]{128}\Z')


def run(*args: str) -> bytes:
    return subprocess.run(args, check=True, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, timeout=30).stdout


def owned_private_file(path: Path) -> bool:
    try:
        mode = path.lstat()
    except FileNotFoundError:
        return False
    return (stat.S_ISREG(mode.st_mode) and mode.st_uid == 0
            and (mode.st_mode & 0o077) == 0)


def enabled() -> bool:
    if os.geteuid() != 0 or not owned_private_file(MARKER):
        return False
    try:
        run('systemd-detect-virt', '--vm', '--quiet')
    except (OSError, subprocess.SubprocessError):
        return False
    return True


def booted_identity() -> dict:
    host = json.loads(run('bootc', 'status', '--json'))
    booted = host['status']['booted']
    image = booted['image']
    digest = image['imageDigest']
    if not SHA256.fullmatch(digest):
        raise ValueError('booted image has no SHA-256 manifest digest')
    reference = image['image']
    name = reference['image']
    transport = reference['transport']
    if not isinstance(name, str) or not name or not isinstance(transport, str) or not transport:
        raise ValueError('booted image reference is missing')
    cmdline = CMDLINE.read_text(encoding='utf-8').split()
    composefs = [token.partition('=')[2] for token in cmdline
                 if token.startswith('composefs=')]
    if len(composefs) != 1 or not COMPOSEFS.fullmatch(composefs[0]):
        raise ValueError('booted composefs identity is missing or ambiguous')
    return {
        'image': name,
        'transport': transport,
        'image_digest': digest,
        'composefs_digest': composefs[0],
        'ostree_checksum': (booted.get('ostree') or {}).get('checksum'),
    }


def cached_image_identity() -> dict:
    details = json.loads(run('podman', 'image', 'inspect', IMAGE))
    if not isinstance(details, list) or len(details) != 1:
        raise ValueError('Podman image inspect returned an unexpected image count')
    image_id = details[0]['Id']
    # Podman returns a bare 64-hex config ID on some builds. It is still an
    # immutable image reference; the raw manifest hash below binds the layers.
    if not PODMAN_IMAGE_ID.fullmatch(image_id):
        raise ValueError('cached image has no SHA-256 ID')
    # A config ID by itself does not commit to the image's layer list.  The raw
    # OCI manifest does, and is cheap to read without exporting layer bytes.
    raw_manifest = run('skopeo', 'inspect', '--raw', f'containers-storage:{IMAGE}')
    manifest = json.loads(raw_manifest)
    if not isinstance(manifest.get('layers'), list) or not manifest['layers']:
        raise ValueError('cached image has no OCI layers')
    return {
        'image_id': image_id,
        'manifest_sha256': hashlib.sha256(raw_manifest).hexdigest(),
    }


def current_receipt() -> dict:
    return {
        'version': 1,
        'booted': booted_identity(),
        'cached': cached_image_identity(),
    }


def can_reuse() -> bool:
    if not enabled() or not owned_private_file(RECEIPT):
        return False
    try:
        recorded = json.loads(RECEIPT.read_text(encoding='utf-8'))
        return recorded == current_receipt()
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        return False


def record() -> None:
    if not enabled():
        raise ValueError('VM cache opt-in is absent')
    receipt = current_receipt()
    try:
        RECEIPT_DIR.mkdir(mode=0o700)
    except FileExistsError:
        pass
    directory = RECEIPT_DIR.lstat()
    if not stat.S_ISDIR(directory.st_mode) or directory.st_uid != 0 \
            or (directory.st_mode & 0o077):
        raise ValueError('receipt directory is not private and root-owned')
    if RECEIPT.exists() and not owned_private_file(RECEIPT):
        raise ValueError('existing receipt is not a private regular root-owned file')
    fd, temporary = tempfile.mkstemp(prefix='.vm-bootc-copy-', dir=RECEIPT_DIR)
    try:
        with os.fdopen(fd, 'w', encoding='utf-8') as output:
            json.dump(receipt, output, sort_keys=True)
            output.write('\n')
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, RECEIPT)
        directory_fd = os.open(RECEIPT_DIR, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def main() -> int:
    if len(sys.argv) != 2 or sys.argv[1] not in ('check', 'record'):
        print('usage: tilo-vm-bootc-copy-cache.py check|record', file=sys.stderr)
        return 2
    if sys.argv[1] == 'check':
        return 0 if can_reuse() else 1
    try:
        record()
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f'VM copy cache receipt unavailable: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
