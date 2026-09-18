#!/usr/bin/env python3
"""Capture a public raw-rootfs handoff; this is not a validation attestation."""
import argparse
import json
import os
from pathlib import Path
import platform
import re

from hardware_profile import development_target


def document(raw_rootfs_id, revision, source_date_epoch, architecture, generation,
             target_update_ref, public_manifest_sha256):
    for value, expression, label in (
        (raw_rootfs_id, r'sha256:[0-9a-f]{64}', 'raw image ID'),
        (revision, r'[0-9a-f]{40}', 'source revision'),
        (public_manifest_sha256, r'[0-9a-f]{64}', 'public manifest digest'),
        (source_date_epoch, r'0|[1-9][0-9]{0,11}', 'source epoch'),
        (generation, r'[1-9][0-9]{0,19}', 'generation'),
    ):
        if not re.fullmatch(expression, value):
            raise ValueError(f'invalid {label}')
    if int(source_date_epoch) > 253402300799 or int(generation) >= 2**64:
        raise ValueError('epoch or generation exceeds supported bounds')
    if architecture not in ('x86_64', 'aarch64') or architecture != platform.machine():
        raise ValueError('raw rootfs handoff requires the native architecture')
    development_target(target_update_ref, architecture)
    return {'schema': 1, 'purpose': 'tilo-development-raw-rootfs-handoff',
            'raw_rootfs_id': raw_rootfs_id, 'revision': revision,
            'source_date_epoch': int(source_date_epoch), 'architecture': architecture,
            'generation': int(generation), 'target_update_ref': target_update_ref,
            'public_manifest_sha256': public_manifest_sha256,
            'installable': False, 'sealed': False, 'published': False,
            'validation': 'captured build inputs only; independently revalidate before reuse'}


def write_exclusive(output, value, checkout):
    output, checkout = Path(output), Path(checkout).resolve(strict=True)
    if not output.is_absolute() or '..' in output.parts or output.is_relative_to(checkout):
        raise ValueError('receipt must be an absolute path outside the checkout')
    # Open every directory without following symlinks. Keep the final dirfd
    # through creation so replacing a parent path cannot redirect the write.
    descriptor = os.open(output.anchor, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for component in output.parts[1:-1]:
            next_descriptor = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                                      dir_fd=descriptor)
            os.close(descriptor)
            descriptor = next_descriptor
        parent = os.fstat(descriptor)
        if parent.st_uid != os.geteuid() or parent.st_mode & 0o022:
            raise ValueError('receipt parent must be owned by the caller and not writable by others')
        file_descriptor = os.open(output.name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                                  0o600, dir_fd=descriptor)
        try:
            with os.fdopen(file_descriptor, 'w') as stream:
                json.dump(value, stream, indent=2, sort_keys=True)
                stream.write('\n')
                stream.flush()
                os.fsync(stream.fileno())
        except BaseException:
            os.unlink(output.name, dir_fd=descriptor)
            raise
    finally:
        os.close(descriptor)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('output', 'raw-rootfs-id', 'revision', 'source-date-epoch', 'architecture',
                 'generation', 'target-update-ref', 'public-manifest-sha256'):
        parser.add_argument('--' + name, required=True)
    args = vars(parser.parse_args())
    output = args.pop('output')
    write_exclusive(output, document(**args), Path(__file__).resolve().parents[2])


if __name__ == '__main__':
    main()
