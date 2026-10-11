#!/usr/bin/env python3
"""Run any worktree's affected selector with verified metadata and no Cargo.

Invoke this stable entry point from outside an older worktree. Its source-bound
CI bundle is validated before the old selector starts; a Python audit hook
then permits only Git subprocesses, rejecting Cargo and alternate launchers.
"""
import argparse
import json
import os
from pathlib import Path
import runpy
import sys
import tempfile

from current_source_metadata import validate

def forbid_local_compilation(event, arguments):
    if event == 'os.system':
        raise RuntimeError('offline affected planning cannot execute shell commands')
    if event not in ('subprocess.Popen', 'os.exec', 'os.posix_spawn'):
        return
    executable = Path(os.fsdecode(arguments[0])).name.lower()
    # The selector's graph is already supplied. Its remaining external work
    # is Git history lookup; deny every other launcher, including absolute
    # Cargo paths and Python/shell indirection into an old metadata fallback.
    if executable != 'git':
        raise RuntimeError('offline affected planning permits only Git subprocesses')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path.cwd())
    parser.add_argument('--bundle', type=Path, required=True)
    parser.add_argument('--base', default='origin/main')
    parser.add_argument('--head', default='HEAD')
    parser.add_argument('--shards', type=int, default=2)
    args = parser.parse_args()
    if args.shards < 1:
        parser.error('shards must be positive')
    root = args.root.resolve()
    # Refuse dirty, stale, partial or mismapped inputs before the caller's
    # historical selector can execute any code or attempt a Cargo fallback.
    metadata = validate(root, json.loads(args.bundle.read_text()))
    selector = root / 'tools/ci/affected_packages.py'
    if not selector.is_file():
        parser.error('worktree has no affected-package selector')
    with tempfile.TemporaryDirectory(prefix='tilo-offline-plan-') as temporary:
        export = Path(temporary) / 'metadata.json'
        export.write_text(json.dumps(metadata))
        sys.addaudithook(forbid_local_compilation)
        os.chdir(root)
        sys.argv = [str(selector), '--metadata', str(export), '--base', args.base,
                    '--head', args.head, '--shards', str(args.shards)]
        # Resolve imports from the selected historical worktree, not from
        # this stable launcher's newer checkout.
        sys.path.insert(0, str(selector.parent))
        runpy.run_path(str(selector), run_name='__main__')
    return 0


if __name__ == '__main__':
    sys.exit(main())
