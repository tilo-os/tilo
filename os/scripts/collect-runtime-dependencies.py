#!/usr/bin/env python3
"""Resolve built ELF dependencies before their files enter a signed runtime.

A dependency is what `ldd` resolves (DT_NEEDED, transitively) and, for the
shared runtime, the libraries it loads on first use: GLib and GStreamer, which
tools/ci/runtime-dylib-lazy-link.py keeps out of DT_NEEDED and names in the
`.tilo.lazy_needed` section. Those are resolved by SONAME and
collected with their own closure, so the signed runtime still carries them.
"""
import argparse
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys

LAZY_SECTION = '.tilo.lazy_needed'
# ldconfig's tag for a library of the running machine.
CACHE_TAGS = {'x86_64': 'x86-64', 'aarch64': 'AArch64'}


def lazy_needed(path):
    """The SONAMEs `path` loads on first use, from its `.tilo.lazy_needed` section."""
    report = subprocess.run(['readelf', '-W', '-p', LAZY_SECTION, str(path)],
                            capture_output=True, text=True)
    if report.returncode:
        raise ValueError(f'readelf cannot read {path}: {report.stderr.strip()}')
    return re.findall(r'^\s*\[\s*[0-9a-f]+\]\s+(\S+)\s*$', report.stdout, re.MULTILINE)


def loader_cache():
    """SONAME -> path for this machine's libraries in the dynamic loader's cache."""
    ldconfig = shutil.which('ldconfig') or next(
        (candidate for candidate in ('/usr/sbin/ldconfig', '/sbin/ldconfig') if Path(candidate).is_file()), None)
    if ldconfig is None:
        raise ValueError('ldconfig is needed to resolve libraries loaded on first use')
    listing = subprocess.run([ldconfig, '-p'], capture_output=True, text=True)
    if listing.returncode:
        raise ValueError(f'ldconfig -p failed: {listing.stderr.strip()}')
    tag = CACHE_TAGS.get(platform.machine(), platform.machine())
    cache = {}
    for match in re.finditer(r'^\s*(\S+)\s+\(([^)]*)\)\s+=>\s+(\S+)\s*$', listing.stdout, re.MULTILINE):
        name, flags, path = match.groups()
        if tag in flags.split(','):
            cache.setdefault(name, Path(path))
    return cache


def locate(soname, library_dirs, cache):
    for directory in library_dirs:
        candidate = directory / soname
        if candidate.is_file():
            return candidate
    if not cache:
        cache.update(loader_cache())
    path = cache.get(soname)
    if path is None or not path.is_file():
        raise ValueError(f'library loaded on first use is unresolved: {soname}')
    return path


def collect(objects, library_dirs):
    if not objects or any(not path.is_file() for path in objects):
        raise ValueError('every dependency input must be an existing ELF file')
    lazy = []
    cache = {}
    for path in objects:
        for soname in lazy_needed(path):
            located = locate(soname, library_dirs, cache)
            if located not in lazy:
                lazy.append(located)
    environment = dict(os.environ, LD_LIBRARY_PATH=':'.join(map(str, library_dirs)))
    report = subprocess.run(['ldd', *map(str, objects), *map(str, lazy)], env=environment,
                            capture_output=True, text=True)
    if report.returncode or 'not found' in report.stdout:
        raise ValueError('native dependencies are unresolved: ' +
                         (report.stdout + report.stderr).strip())
    paths = set(lazy)
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
