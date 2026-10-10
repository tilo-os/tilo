#!/usr/bin/env python3
"""Run a bounded list of exact tests in one target, or compile package tests."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys


def request(env):
    values = {key: env.get(key, '') for key in
              ('SOURCE_SHA', 'PACKAGE', 'PACKAGES', 'TEST_TARGET', 'TEST_NAME', 'TEST_NAMES', 'OPERATION')}
    values['TARGET_KIND'] = env.get('TARGET_KIND', '') or 'integration'
    count = env.get('REPEAT_COUNT', '')
    if count == '':
        count = '1'
    if not isinstance(count, str) or not re.fullmatch(r'(?:[1-9]|1[0-9]|20)', count):
        raise ValueError('repeat count must be an integer between 1 and 20')
    values['REPEAT_COUNT'] = int(count)
    if values['REPEAT_COUNT'] != 1 and values['OPERATION'] not in ('test', 'test-lib'):
        raise ValueError('only exact tests can be repeated')
    if not re.fullmatch(r'[0-9a-f]{40}', values['SOURCE_SHA']):
        raise ValueError('source must be a full lowercase commit SHA')
    if values['PACKAGES']:
        if values['PACKAGE'] or values['OPERATION'] != 'check-tests' or len(values['PACKAGES']) > 16_384:
            raise ValueError('package sets are bounded check-tests requests, mutually exclusive with package')
        names = json.loads(values['PACKAGES'])
        if not isinstance(names, list) or not 1 <= len(names) <= 64:
            raise ValueError('request between 1 and 64 workspace packages')
    else:
        names = [values['PACKAGE']]
    if any(not isinstance(name, str) or not re.fullmatch(
            r'[A-Za-z0-9][A-Za-z0-9_-]{0,99}', name) for name in names):
        raise ValueError('package must be a Cargo package name')
    if len(set(names)) != len(names):
        raise ValueError('duplicate package names are not allowed')
    values['PACKAGE_NAMES'] = names
    if values['OPERATION'] not in ('test', 'test-lib', 'check-tests'):
        raise ValueError('operation must be test, test-lib or check-tests')
    if values['OPERATION'] == 'test-lib':
        if values['TEST_TARGET']:
            raise ValueError('test-lib does not accept an integration target')
        if env.get('TARGET_KIND', '') not in ('', 'lib'):
            raise ValueError('test-lib requires the lib target kind')
        values['TARGET_KIND'] = 'lib'
    if values['TARGET_KIND'] not in ('integration', 'lib'):
        raise ValueError('target kind must be integration or lib')
    if values['OPERATION'] in ('test', 'test-lib'):
        if values['TARGET_KIND'] == 'integration' and not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]{0,99}', values['TEST_TARGET']):
            raise ValueError('an integration test target is required')
        if values['TARGET_KIND'] == 'lib' and values['TEST_TARGET']:
            raise ValueError('lib tests do not take an integration target')
        if values['TEST_NAMES']:
            if values['TEST_NAME'] or len(values['TEST_NAMES']) > 16_384:
                raise ValueError('use one test_name or a bounded test_names JSON array')
            cases = json.loads(values['TEST_NAMES'])
        else:
            cases = [values['TEST_NAME']]
        if not isinstance(cases, list) or not 1 <= len(cases) <= 32:
            raise ValueError('request between 1 and 32 exact tests')
        for case in cases:
            if not isinstance(case, str) or len(case) > 512 or not re.fullmatch(
                    r'[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*', case):
                raise ValueError('an exact Rust test path is required')
        if len(set(cases)) != len(cases):
            raise ValueError('duplicate test names are not allowed')
        values['TEST_CASES'] = cases
    elif values['TEST_TARGET'] or values['TEST_NAME'] or values['TEST_NAMES']:
        raise ValueError('check-tests does not accept a test target or name')
    return values


def ancestor_cargo_configs(root):
    """Cargo configuration files the source would inherit from a parent.

    The filesystem root is excluded: the CI builder image installs its
    toolchain settings (the default native linker) in /.cargo/config.toml so
    that every job reads them, and that file belongs to the image, not to a
    checkout of another revision.
    """
    found = []
    for parent in root.resolve().parents:
        if parent == Path(parent.anchor):
            continue
        found.extend(parent / '.cargo' / name for name in ('config', 'config.toml')
                     if (parent / '.cargo' / name).exists())
    return found


def execute(root, values, outcomes=None, on_outcome=None):
    # Cargo merges configuration from ancestors. The workflow helper checkout
    # must be a sibling, never a parent supplying another revision's settings.
    if ancestor_cargo_configs(root):
        raise ValueError('source inherits an ancestor Cargo configuration')
    def read(args):
        return subprocess.check_output(args, cwd=root, text=True)
    actual = read(['git', 'rev-parse', 'HEAD']).strip()
    if actual != values['SOURCE_SHA']:
        raise ValueError('checked-out source differs from requested SHA')
    metadata = json.loads(read(['cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1']))
    names = values.get('PACKAGE_NAMES', [values['PACKAGE']])
    packages = []
    for name in names:
        matches = [p for p in metadata['packages']
                   if p['name'] == name and p['id'] in metadata['workspace_members']]
        if len(matches) != 1:
            raise ValueError('package is not a unique workspace member: ' + name)
        packages.extend(matches)
    if values['OPERATION'] == 'check-tests':
        command = ['cargo', 'check', '--locked',
                   *[arg for name in names for arg in ('-p', name)], '--tests']
        print(json.dumps({'source_sha': actual, 'command': command}), flush=True)
        return subprocess.run(command, cwd=root).returncode
    if values['OPERATION'] == 'test-lib' or values.get('TARGET_KIND', 'integration') == 'lib':
        library_kinds = {'lib', 'rlib', 'dylib', 'cdylib', 'staticlib', 'proc-macro'}
        if not any(library_kinds.intersection(t['kind']) for t in packages[0]['targets']):
            raise ValueError('package has no library test target')
        target = ['--lib']
    else:
        if not any(t['name'] == values['TEST_TARGET'] and 'test' in t['kind']
                   for t in packages[0]['targets']):
            raise ValueError('integration test target does not belong to package')
        target = ['--test', values['TEST_TARGET']]
    prefix = ['cargo', 'test', '--locked', '-p', values['PACKAGE'], *target]
    # libtest succeeds with zero selected tests. Reject that before executing,
    # and reject ignored tests afterwards rather than reporting false evidence.
    listed = read(prefix + ['--', '--list'])
    cases = values.get('TEST_CASES', [values['TEST_NAME']])
    for case in cases:
        if listed.splitlines().count(case + ': test') != 1:
            raise ValueError('exact test is absent or ambiguous in the compiled target: ' + case)
    outcomes = [] if outcomes is None else outcomes
    code = 0
    for iteration in range(1, values.get('REPEAT_COUNT', 1) + 1):
        for case in cases:
            command = prefix + ['--', case, '--exact', '--nocapture']
            print(json.dumps({'source_sha': actual, 'command': command, 'iteration': iteration}), flush=True)
            # Compile once; each invocation keeps Cargo's source-owned runner
            # and its private runtime environment. Test budgets are unchanged.
            result = subprocess.run(command, cwd=root, text=True, stdout=subprocess.PIPE)
            print(result.stdout, end='', flush=True)
            executed = bool(re.search(r'test result: (?:ok|FAILED)\. (?:1 passed; 0 failed|0 passed; 1 failed); 0 ignored;', result.stdout))
            passed = result.returncode == 0 and bool(re.search(
                r'test result: ok\. 1 passed; 0 failed; 0 ignored;', result.stdout))
            outcome = {'name': case, 'exit_code': result.returncode,
                       'executed': executed, 'passed': passed, 'iteration': iteration}
            outcomes.append(outcome)
            if on_outcome is not None:
                on_outcome()
            print('TILO_FOCUSED_CASE ' + json.dumps(outcome, sort_keys=True), flush=True)
            if not passed:
                code = code or result.returncode or 1
        if code:
            break
    return code


def save_receipt(path, receipt):
    if path is None:
        return
    cases = receipt.get('TEST_CASES', [])
    receipt['completed_iterations'] = len(receipt.get('cases', [])) // len(cases) if cases else 0
    data = json.dumps(receipt, indent=2) + '\n'
    if len(data.encode()) > 1_048_576:
        raise ValueError('focused receipt exceeds its one MiB bound')
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix('.partial')
    temporary.write_text(data)
    temporary.replace(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('validate', 'run'))
    parser.add_argument('--root', type=Path, default=Path('source'))
    parser.add_argument('--receipt', type=Path, help='workflow-owned bounded progress artifact')
    args = parser.parse_args()
    receipt = {'schema': 'tilo.focused-test/v1', 'workflow_sha': os.environ.get('GITHUB_SHA'),
               'builder_image': os.environ.get('BUILDER_IMAGE'), 'qualification': 'supplemental-exact-tests'}
    code = 1
    try:
        values = request(os.environ)
        receipt.update(values)
        if args.mode == 'validate':
            with Path(os.environ['GITHUB_OUTPUT']).open('a') as out:
                out.write('source=' + values['SOURCE_SHA'] + '\n')
            return 0
        receipt['cases'] = []
        receipt['exit_code'] = None
        receipt['complete'] = False
        save_receipt(args.receipt, receipt)
        code = execute(args.root.resolve(), values, receipt['cases'],
                       lambda: save_receipt(args.receipt, receipt))
    except (ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(str(error), file=sys.stderr)
        receipt['error'] = str(error)
        if isinstance(error, subprocess.CalledProcessError):
            code = error.returncode
    cases = receipt.get('TEST_CASES', [])
    if code == 0 and cases and len(receipt.get('cases', [])) != len(cases) * receipt['REPEAT_COUNT']:
        code = 1
        receipt['error'] = 'requested exact-test iterations did not complete'
    receipt['exit_code'] = code
    receipt['complete'] = code == 0
    save_receipt(args.receipt, receipt)
    print('TILO_FOCUSED_TEST ' + json.dumps(receipt, sort_keys=True))
    if os.environ.get('GITHUB_STEP_SUMMARY'):
        with Path(os.environ['GITHUB_STEP_SUMMARY']).open('a') as out:
            out.write('```json\n' + json.dumps(receipt, indent=2) + '\n```\n')
    return code if code >= 0 else 1


if __name__ == '__main__':
    sys.exit(main())
