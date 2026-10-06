#!/usr/bin/env python3
"""Copy handoff contents without importing private staging-directory metadata."""
import argparse
import os
from pathlib import Path
import shutil


def copy_tree(source, destination):
    if source.is_symlink() or not source.is_dir():
        raise ValueError('handoff source must be a real directory')
    if destination.is_symlink() or not destination.is_dir():
        raise ValueError('installed destination must be a real directory')
    for entry in sorted(source.iterdir()):
        target = destination / entry.name
        if entry.is_symlink() or target.is_symlink():
            raise ValueError('handoff paths must not contain symlinks')
        if entry.is_dir():
            if not target.exists():
                target.mkdir(mode=0o755)
                target.chmod(0o755)
            copy_tree(entry, target)
        elif entry.is_file():
            if target.exists() and not target.is_file():
                raise ValueError('handoff destination is not a regular file')
            # Existing directory ownership, modes and SELinux labels remain
            # intact. New files start private; callers apply the explicit
            # NetworkManager, identity-key and public SSH-key policies.
            descriptor = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_TRUNC
                                 | os.O_NOFOLLOW, 0o600)
            with os.fdopen(descriptor, 'wb') as output, entry.open('rb') as stream:
                os.fchmod(output.fileno(), 0o600)
                shutil.copyfileobj(stream, output)
        else:
            raise ValueError('handoff source is not a regular file or directory')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('destination', type=Path)
    args = parser.parse_args()
    copy_tree(args.source, args.destination)
