#!/usr/bin/env python3
"""Run one explicitly named integration test or compile one package's tests."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys


def request(env):
    values = {key: env.get(key, '') for key in
              ('SOURCE_SHA', 'PACKAGE', 'TEST_TARGET', 'TEST_NAME', 'OPERATION')}
    if not re.fullmatch(r'[0-9a-f]{40}', values['SOURCE_SHA']):
        raise ValueError('source must be a full lowercase commit SHA')
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]{0,99}', values['PACKAGE']):
        raise ValueError('package must be a Cargo package name')
    if values['OPERATION'] not in ('test', 'check-tests'):
        raise ValueError('operation must be test or check-tests')
    if values['OPERATION'] == 'test':
        if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]{0,99}', values['TEST_TARGET']):
            raise ValueError('an integration test target is required')
        if not re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*', values['TEST_NAME']):
            raise ValueError('an exact Rust test path is required')
    elif values['TEST_TARGET'] or values['TEST_NAME']:
        raise ValueError('check-tests does not accept a test target or name')
    return values


def execute(root, values):
    def read(args):
        return subprocess.check_output(args, cwd=root, text=True)
    actual = read(['git', 'rev-parse', 'HEAD']).strip()
    if actual != values['SOURCE_SHA']:
        raise ValueError('checked-out source differs from requested SHA')
    metadata = json.loads(read(['cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1']))
    packages = [p for p in metadata['packages']
                if p['name'] == values['PACKAGE'] and p['id'] in metadata['workspace_members']]
    if len(packages) != 1:
        raise ValueError('package is not a unique workspace member')
    if values['OPERATION'] == 'check-tests':
        command = ['cargo', 'check', '--locked', '-p', values['PACKAGE'], '--tests']
        print(json.dumps({'source_sha': actual, 'command': command}), flush=True)
        return subprocess.run(command, cwd=root).returncode
    if not any(t['name'] == values['TEST_TARGET'] and 'test' in t['kind']
               for t in packages[0]['targets']):
        raise ValueError('integration test target does not belong to package')
    command = ['cargo', 'test', '--locked', '-p', values['PACKAGE'],
               '--test', values['TEST_TARGET'], '--', values['TEST_NAME'], '--exact']
    # libtest succeeds with zero selected tests. Reject that before executing,
    # and reject ignored tests afterwards rather than reporting false evidence.
    listed = read(command + ['--list'])
    if listed.splitlines().count(values['TEST_NAME'] + ': test') != 1:
        raise ValueError('exact test is absent or ambiguous in the compiled target')
    print(json.dumps({'source_sha': actual, 'command': command}), flush=True)
    result = subprocess.run(command + ['--nocapture'], cwd=root, text=True, stdout=subprocess.PIPE)
    print(result.stdout, end='', flush=True)
    if result.returncode:
        return result.returncode
    if not re.search(r'test result: ok\. 1 passed; 0 failed; 0 ignored;', result.stdout):
        raise ValueError('test did not execute exactly one passing, non-ignored case')
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('validate', 'run'))
    parser.add_argument('--root', type=Path, default=Path('source'))
    args = parser.parse_args()
    receipt = {'schema': 'tilo.focused-test/v1', 'workflow_sha': os.environ.get('GITHUB_SHA'),
               'builder_image': os.environ.get('BUILDER_IMAGE')}
    code = 1
    try:
        values = request(os.environ)
        receipt.update(values)
        if args.mode == 'validate':
            with Path(os.environ['GITHUB_OUTPUT']).open('a') as out:
                out.write('source=' + values['SOURCE_SHA'] + '\n')
            return 0
        code = execute(args.root.resolve(), values)
    except (ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(str(error), file=sys.stderr)
        receipt['error'] = str(error)
        if isinstance(error, subprocess.CalledProcessError):
            code = error.returncode
    receipt['exit_code'] = code
    print('TILO_FOCUSED_TEST ' + json.dumps(receipt, sort_keys=True))
    if os.environ.get('GITHUB_STEP_SUMMARY'):
        with Path(os.environ['GITHUB_STEP_SUMMARY']).open('a') as out:
            out.write('```json\n' + json.dumps(receipt, indent=2) + '\n```\n')
    return code if code >= 0 else 1


if __name__ == '__main__':
    sys.exit(main())
