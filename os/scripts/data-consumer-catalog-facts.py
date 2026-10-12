#!/usr/bin/python3
"""Export checked installed facts for the release signer's catalog/manifest.

Run in the exact final native target root, before freezing/publishing its image.
This does not sign anything: fields must travel in the existing authenticated
catalog or internal manifest, never as a freestanding trusted sidecar.
"""
import importlib.util
import json
import os
from pathlib import Path
import stat
import sys


def main():
    if sys.argv[1:]:
        raise ValueError('catalog fact exporter accepts no arguments')
    path = Path('/usr/libexec/tilo-installed-data-consumer.py')
    for edge in (Path('/'), Path('/usr'), path.parent, path):
        info = edge.lstat()
        if info.st_uid != 0 or info.st_mode & 0o022 or stat.S_ISLNK(info.st_mode):
            raise ValueError('catalog fact exporter dependency is not image-owned')
    if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= 1024 * 1024:
        raise ValueError('catalog fact exporter dependency exceeds bound')
    spec = importlib.util.spec_from_file_location('installed_data_consumer', path)
    consumer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(consumer)
    store = consumer.Store()
    facts = {'home_archive': consumer.home(store, check=True)}
    app = store.path('/usr/share/tilo/' + consumer.APPS)
    native = store.path('/usr/share/tilo/' + consumer.NATIVE)
    # An incomplete attempted native producer fails publication, not silently
    # downgrades the release to legacy support.
    if os.path.lexists(app) or os.path.lexists(native):
        facts['applications'] = consumer.applications(store, check=True)
        facts['original_source'] = store.receipt(consumer.ORIGINAL, 8192)
    print(consumer.canonical(facts).decode())


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise SystemExit('consumer catalog facts unavailable: ' + str(error)) from error
