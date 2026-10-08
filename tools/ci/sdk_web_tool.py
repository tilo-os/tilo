#!/usr/bin/env python3
"""Transfer a browser build tool within one workflow run, never across runs.

Both consumers still build and qualify their own products. No Cargo target tree
or cache is transferred; the native CLI is bound to the merged Git tree, builder
image, host architecture and workflow run, then checked against the producer's
output digest before it can execute.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def binding() -> dict:
    builder = os.environ['TILO_CI_BUILDER']
    if not re.fullmatch(r'[^\s]+@sha256:[0-9a-f]{64}', builder):
        raise ValueError('builder must be immutable')
    run = os.environ['GITHUB_RUN_ID']
    if not run.isdigit():
        raise ValueError('missing workflow run identity')
    tree = subprocess.check_output(['git', 'rev-parse', 'HEAD^{tree}'], text=True).strip()
    return {'schema': 'tilo.sdk-web-tool/v1', 'builder': builder, 'run': run,
            'tree': tree, 'architecture': platform.machine()}


def stage(directory: Path, context: dict) -> str:
    directory.mkdir(parents=True, exist_ok=False)
    tool = directory / 'tilo-app'
    shutil.copyfile('target/debug/tilo-app', tool)
    digest = sha256(tool)
    (directory / 'binding.json').write_text(json.dumps({**context, 'sha256': digest}, sort_keys=True) + '\n')
    return digest


def install(directory: Path, context: dict, expected: str) -> str:
    if not re.fullmatch(r'[0-9a-f]{64}', expected):
        raise ValueError('missing producer digest')
    tool = directory / 'tilo-app'
    receipt = json.loads((directory / 'binding.json').read_text())
    if receipt != {**context, 'sha256': expected}:
        raise ValueError('tool does not belong to this tree, builder, architecture and run')
    if tool.is_symlink() or not tool.is_file() or sha256(tool) != expected:
        raise ValueError('tool bytes differ from the producer')
    destination = Path('target/debug/tilo-app')
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_suffix('.incoming')
    shutil.copyfile(tool, temporary)
    if sha256(temporary) != expected:
        temporary.unlink()
        raise ValueError('tool changed during installation')
    temporary.chmod(0o755)
    temporary.replace(destination)
    return expected


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('stage', 'install'))
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    context = binding()
    if args.command == 'stage':
        digest = stage(args.directory, context)
        with open(os.environ['GITHUB_OUTPUT'], 'a', encoding='utf-8') as output:
            output.write(f'sha256={digest}\n')
    else:
        digest = install(args.directory, context, os.environ['TILO_CI_TOOL_SHA256'])
    print(f'sdk-web-tool {args.command}: sha256={digest}; tree={context["tree"]}; run={context["run"]}')


if __name__ == '__main__':
    main()
