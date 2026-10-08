#!/usr/bin/env python3
"""Transfer a browser build tool within one workflow run, never across runs.

Both consumers still build and qualify their own products. No Cargo target tree
or cache is transferred; the native CLI is bound to the merged Git tree, builder
image, host architecture and workflow run, then checked against the producer's
manifest digest before it can execute. An already-finalized artifact may be reused
opportunistically; a miss leaves the original local build in place.
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
import urllib.error
from urllib.request import Request, urlopen


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
            'tree': tree, 'architecture': platform.machine(),
            'producer': 'linux', 'profile': 'dev', 'debuginfo': '0'}


def stage(directory: Path, context: dict) -> str:
    if os.environ.get('CARGO_PROFILE_DEV_DEBUG') != '0':
        raise ValueError('Linux producer must use the recorded debug-symbol setting')
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


def probe(context: dict) -> bool:
    """One bounded metadata read; never wait for the producer to start or finish."""
    repository = os.environ['GITHUB_REPOSITORY']
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository):
        raise ValueError('invalid repository identity')
    url = (f"{os.environ['GITHUB_SERVER_URL']}/api/v1/repos/{repository}"
           f"/actions/runs/{context['run']}/artifacts")
    request = Request(url, headers={'Authorization': f"token {os.environ['GITHUB_TOKEN']}",
                                    'Accept': 'application/json'})
    try:
        with urlopen(request, timeout=2) as response:
            data = response.read(128 * 1024 + 1)
        if len(data) > 128 * 1024:
            raise ValueError('artifact metadata exceeded its bound')
        artifacts = json.loads(data)
        if not isinstance(artifacts, list):
            raise ValueError('unexpected artifact metadata shape')
        return any(isinstance(a, dict) and a.get('name') == 'app-sdk-web-tool' and not a.get('expired', True)
                   and a.get('run_id') == int(context['run']) and a.get('size_in_bytes', 0) > 0
                   for a in artifacts)
    except (OSError, ValueError, TypeError):
        # Reuse is optional; the caller still compiles the tool and runs every
        # product check. No denied read is retried and no credential is logged.
        print('sdk-web-tool: metadata unavailable; retain the local build')
        return False


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('stage', 'install', 'probe', 'reuse'))
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    context = binding()
    if args.command == 'probe':
        ready = probe(context)
        with open(os.environ['GITHUB_OUTPUT'], 'a', encoding='utf-8') as output:
            output.write(f'ready={str(ready).lower()}\n')
        print(f'sdk-web-tool: already available={ready}; no producer wait')
        return
    if args.command == 'stage':
        digest = stage(args.directory, context)
        with open(os.environ['GITHUB_OUTPUT'], 'a', encoding='utf-8') as output:
            output.write(f'sha256={digest}\n')
    else:
        expected = (json.loads((args.directory / 'binding.json').read_text())['sha256']
                    if args.command == 'reuse' else os.environ['TILO_CI_TOOL_SHA256'])
        digest = install(args.directory, context, expected)
    print(f'sdk-web-tool {args.command}: sha256={digest}; tree={context["tree"]}; run={context["run"]}')


if __name__ == '__main__':
    main()
