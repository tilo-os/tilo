"""Fixed, bounded diagnostic of the production mounted-resource acceptance path."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import time


def request(environment):
    source = environment.get('SOURCE_SHA', '')
    if not re.fullmatch(r'[0-9a-f]{40}', source):
        raise ValueError('source must be an exact lowercase commit SHA')
    return source


def execute(root, source):
    root = root.resolve()
    actual = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
    if actual != source:
        raise ValueError('checked-out source differs from requested SHA')
    evidence = root / 'target/mounted-resource-recovery'
    output = root / 'target/mounted-reopen-probe'
    if evidence.exists() or output.exists():
        raise ValueError('diagnostic evidence already exists; preserve it before a new run')
    output.mkdir(parents=True, mode=0o700)
    # Cargo's build directory; the fixture keeps its evidence under ./target.
    build = Path(os.environ.get('CARGO_TARGET_DIR') or root / 'target')
    commands = (
        ['cargo', 'build', '--locked', '-p', 'tilo-app-cli', '--bin', 'tilo-app'],
        [str(build / 'debug/tilo-app'), 'build', 'apps/chat', '--platform', 'web'],
        ['cargo', 'build', '--locked', '-p', 'tilo-agent-participant', '--example', 'mounted-resource'],
    )
    for command in commands:
        code = subprocess.run(command, cwd=root).returncode
        if code:
            return code
    # Reuse the unchanged fixture and its 130-second bound. This is not a retry
    # of a failed Forgejo run: the first failure ends this diagnostic immediately.
    command = ['cargo', 'run', '--locked', '-p', 'tilo-agent-participant',
               '--example', 'mounted-resource', '--',
               str(build / 'tilo-app/web/chat/bundle'), '--disconnect-retry']
    for attempt in range(1, 4):
        started = time.monotonic()
        saved = output / f'attempt-{attempt}'
        code = 1
        try:
            code = subprocess.run(command, cwd=root).returncode
        finally:
            if evidence.exists():
                evidence.rename(saved)
            else:
                saved.mkdir(mode=0o700)
            receipt = {'source_sha': source, 'builder_image': os.environ.get('BUILDER_IMAGE'),
                       'attempt': attempt, 'exit_code': code,
                       'seconds': time.monotonic() - started, 'command': command}
            (saved / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
            print(json.dumps(receipt), flush=True)
        if code:
            return code
        journey = saved / 'journey.json'
        if not journey.is_file() or journey.stat().st_size > 65536:
            raise ValueError('successful process did not produce bounded journey evidence')
        result = json.loads(journey.read_text())
        if (result.get('mode') != 'certified-browser-disconnect-retry'
                or result.get('stages', [])[-1:] != ['governed-revocation-refuses-link-reopen']):
            raise ValueError('diagnostic did not complete the disconnect/retry journey')
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=('validate', 'run'))
    parser.add_argument('--root', type=Path, default=Path('.'))
    args = parser.parse_args()
    source = request(os.environ)
    if args.operation == 'validate':
        with Path(os.environ['GITHUB_OUTPUT']).open('a') as output:
            output.write('source=' + source + '\n')
        return 0
    return execute(args.root, source)


if __name__ == '__main__':
    raise SystemExit(main())
