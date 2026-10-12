#!/usr/bin/python3
"""Installed applications consumer producer; --check is read-only and rootless."""
import importlib.util
import os
from pathlib import Path
import stat
import sys


def main():
    if sys.argv[1:] not in ([], ['--check']):
        raise ValueError('consumer verifier accepts only --check')
    path = Path('/usr/libexec/tilo-installed-data-consumer.py')
    for edge in (Path('/'), Path('/usr'), path.parent, path):
        info = edge.lstat()
        if info.st_uid != 0 or info.st_mode & 0o022 or stat.S_ISLNK(info.st_mode):
            raise ValueError('consumer verifier module is not image-owned')
    if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= 1024 * 1024:
        raise ValueError('consumer verifier module exceeds bound')
    spec = importlib.util.spec_from_file_location('installed_data_consumer', path)
    consumer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(consumer)
    consumer.main('applications', sys.argv[1:])


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise SystemExit('installed applications consumer unavailable: ' + str(error)) from error
