#!/usr/bin/env python3
"""Select the tools recipe from the independently pinned application checkout."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

RECIPE = 'os/Containerfile.development-compile'


def git(directory, *arguments):
    return subprocess.check_output(['git', '-C', str(directory), *arguments],
                                   stderr=subprocess.DEVNULL, timeout=30)


def stage(controller, controller_revision, source, source_revision, output, receipt):
    controller = Path(controller).resolve()
    if bool(source) != bool(source_revision):
        raise ValueError('separate source requires its independent revision')
    selected = Path(source).resolve() if source else controller
    revision = source_revision or controller_revision
    for directory, expected in ((controller, controller_revision), (selected, revision)):
        if not re.fullmatch(r'[0-9a-f]{40}', expected or ''):
            raise ValueError('immutable checkout revision required')
        if (git(directory, 'rev-parse', 'HEAD').decode().strip() != expected
                or git(directory, 'status', '--porcelain', '--untracked-files=all').strip()):
            raise ValueError('tools source must be clean and match its pinned revision')
    recipe = selected / RECIPE
    if recipe.is_symlink() or not recipe.is_file() or recipe.stat().st_size > 65536:
        raise ValueError('bounded regular tools recipe required')
    data = git(selected, 'show', revision + ':' + RECIPE)
    if not 0 < len(data) <= 65536 or recipe.read_bytes() != data:
        raise ValueError('tools recipe bytes differ from pinned Git blob')
    value = {'schema': 1, 'purpose': 'assembly-tools-source-selection',
             'controller_revision': controller_revision, 'application_revision': revision,
             'recipe': RECIPE, 'recipe_sha256': hashlib.sha256(data).hexdigest(),
             'selected_application_checkout': bool(source), 'tools_built': False}
    with Path(output).open('xb') as stream:
        stream.write(data)
    with Path(receipt).open('x', encoding='utf-8') as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.write('\n')
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('controller', 'controller-revision', 'output', 'receipt'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--source')
    parser.add_argument('--source-revision', default='')
    args = parser.parse_args()
    print(json.dumps(stage(args.controller, args.controller_revision, args.source, args.source_revision,
                           args.output, args.receipt), sort_keys=True))


if __name__ == '__main__':
    try: main()
    except (OSError, ValueError, subprocess.SubprocessError):
        raise SystemExit('tools recipe source selection rejected before build')
