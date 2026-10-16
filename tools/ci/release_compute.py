#!/usr/bin/env python3
"""Stage a release bundle's private image worker with its hashes, and verify them.

  release_compute.py stage BUNDLE OUT   copy the worker's files and write sha256.json
  release_compute.py verify DIR         fail unless DIR holds exactly those files, unchanged
"""
import hashlib
import json
import pathlib
import sys

NAMES = ('compute-worker.mjs', 'compute-domains.mjs', 'compute-protocol.mjs',
         'pkg/compute/tilo_browser_compute.js', 'pkg/compute/tilo_browser_compute_bg.wasm')


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def stage(product, output):
    output.mkdir(parents=True)
    hashes = {}
    for name in NAMES:
        data = (product / name).read_bytes()
        destination = output / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
        hashes[name] = hashlib.sha256(data).hexdigest()
        assert digest(destination) == hashes[name], name
    (output / 'sha256.json').write_text(json.dumps(hashes, sort_keys=True) + '\n')


def verify(product):
    hashes = json.loads((product / 'sha256.json').read_text())
    assert set(hashes) == set(NAMES), 'incomplete release worker manifest'
    for name in NAMES:
        assert digest(product / name) == hashes[name], name


if __name__ == '__main__':
    if sys.argv[1:2] == ['stage'] and len(sys.argv) == 4:
        stage(pathlib.Path(sys.argv[2]), pathlib.Path(sys.argv[3]))
    elif sys.argv[1:2] == ['verify'] and len(sys.argv) == 3:
        verify(pathlib.Path(sys.argv[2]))
    else:
        raise SystemExit(__doc__)
