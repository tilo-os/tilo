#!/usr/bin/env python3
"""Resolve built ELF dependencies before their files enter a signed runtime."""
import argparse
import os
from pathlib import Path
import re
import subprocess
import sys


def collect(objects, library_dirs):
    if not objects or any(not path.is_file() for path in objects):
        raise ValueError('every dependency input must be an existing ELF file')
    environment = dict(os.environ, LD_LIBRARY_PATH=':'.join(map(str, library_dirs)))
    report = subprocess.run(['ldd', *map(str, objects)], env=environment,
                            capture_output=True, text=True)
    if report.returncode or 'not found' in report.stdout:
        raise ValueError('native dependencies are unresolved: ' +
                         (report.stdout + report.stderr).strip())
    paths = set()
    for match in re.finditer(r'(?:=>\s*)?(/[^\s()]+)', report.stdout):
        value = match.group(1)
        if value.endswith(':'):
            continue  # ldd's heading for an input object, not a dependency.
        path = Path(value)
        if not path.is_file():
            raise ValueError(f'resolved native dependency is absent: {path}')
        paths.add(path)
    if not paths:
        raise ValueError('no native dependencies were observed')
    return sorted(paths)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--library-dir', type=Path, action='append', default=[])
    parser.add_argument('objects', type=Path, nargs='+')
    args = parser.parse_args()
    try:
        for path in collect(args.objects, args.library_dir):
            print(path)
    except ValueError as error:
        print(f'collect-runtime-dependencies: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
