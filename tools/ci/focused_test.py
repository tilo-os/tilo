#!/usr/bin/env python3
"""Run a bounded list of exact tests in one target, or compile package tests."""
import argparse
import json
import os
from pathlib import Path
import re
import resource
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile


HISTORY_BACKTRACE_OPERATION = 'history-backtrace'
HISTORY_BACKTRACE_PACKAGE = 'qos-browser-runtime'
HISTORY_BACKTRACE_CASES = (
    'desktop_host::tests::external_history_opens_retained_framework_without_changing_heads',
    'desktop_host::tests::workspace_navigation_document_history_interlock_preserves_exact_owner',
)
HISTORY_BACKTRACE_RUNNER = 'CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER'


def request(env):
    values = {key: env.get(key, '') for key in
              ('SOURCE_SHA', 'PACKAGE', 'TEST_TARGET', 'TEST_NAME', 'TEST_NAMES', 'OPERATION')}
    values['TARGET_KIND'] = env.get('TARGET_KIND', '') or 'integration'
    if not re.fullmatch(r'[0-9a-f]{40}', values['SOURCE_SHA']):
        raise ValueError('source must be a full lowercase commit SHA')
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]{0,99}', values['PACKAGE']):
        raise ValueError('package must be a Cargo package name')
    if values['OPERATION'] not in ('test', 'test-lib', 'check-tests', 'inspect-native',
                                   HISTORY_BACKTRACE_OPERATION):
        raise ValueError('operation must be test, test-lib, check-tests, inspect-native or history-backtrace')
    if values['OPERATION'] == 'test-lib':
        if values['TEST_TARGET']:
            raise ValueError('test-lib does not accept an integration target')
        if env.get('TARGET_KIND', '') not in ('', 'lib'):
            raise ValueError('test-lib requires the lib target kind')
        values['TARGET_KIND'] = 'lib'
    if values['TARGET_KIND'] not in ('integration', 'lib'):
        raise ValueError('target kind must be integration or lib')
    if values['OPERATION'] == HISTORY_BACKTRACE_OPERATION:
        if (values['PACKAGE'] != HISTORY_BACKTRACE_PACKAGE or values['TARGET_KIND'] != 'lib'
                or values['TEST_TARGET'] or values['TEST_NAME'] or values['TEST_NAMES']):
            raise ValueError('history-backtrace is fixed to qos-browser-runtime lib history cases')
        values['TEST_CASES'] = list(HISTORY_BACKTRACE_CASES)
    elif values['OPERATION'] in ('test', 'test-lib'):
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
        raise ValueError('this operation does not accept a test target or name')
    return values


def inspect_native_environment(root=Path('/')):
    """Observe this ordinary job container; never open a device or run a tool."""
    def bounded_text(relative):
        try:
            with (root / relative).open() as source:
                return source.read(65536)
        except OSError:
            return ''
    status = bounded_text('proc/self/status')
    capabilities = re.search(r'^CapEff:\s*([0-9a-fA-F]+)$', status, re.MULTILINE)
    nodes = []
    for name in ['loop-control', *('loop' + str(n) for n in range(256))]:
        try:
            metadata = (root / 'dev' / name).lstat()
        except FileNotFoundError:
            continue
        nodes.append({'name': name, 'type': stat.S_IFMT(metadata.st_mode),
                      'mode': stat.S_IMODE(metadata.st_mode),
                      'major': os.major(metadata.st_rdev), 'minor': os.minor(metadata.st_rdev)})
    return {'scope': 'normal-job-container',
            'filesystems': bounded_text('proc/filesystems').splitlines(),
            'effective_capabilities': capabilities.group(1) if capabilities else None,
            'loop_nodes': nodes,
            'tools': {name: shutil.which(name) for name in ('gdb', 'btrfs', 'mkfs.btrfs', 'losetup')},
            'mount_or_device_write_attempted': False,
            'nested_privileged_container_tested': False}


def history_test_binary(path, root):
    try:
        binary = Path(path).resolve(strict=True)
    except OSError as error:
        raise ValueError('Cargo runner supplied an unavailable test binary') from error
    target = (root / 'target').resolve()
    try:
        binary.relative_to(target)
    except ValueError as error:
        raise ValueError('Cargo runner supplied a binary outside the source target directory') from error
    mode = binary.stat().st_mode
    if not stat.S_ISREG(mode) or not mode & 0o111:
        raise ValueError('Cargo runner supplied a non-executable test binary')
    return binary


def history_gdb_runner(arguments, root):
    if not arguments:
        raise ValueError('Cargo runner did not supply a test binary')
    binary = history_test_binary(arguments[0], root)
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    command = [
        'gdb', '--nx', '--batch', '--return-child-result',
        '-ex', 'set pagination off',
        '-ex', 'set print frame-arguments none',
        '-ex', 'run', '-ex', 'thread apply all bt', '--args', str(binary), *arguments[1:],
    ]
    print('TILO_HISTORY_GDB ' + json.dumps({'command': command}, sort_keys=True), flush=True)
    result = subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    print(result.stdout, end='', flush=True)
    if result.returncode == 0 and re.search(
            r'^(?:Program received signal|Program terminated with signal|Thread [0-9]+ .* received signal)\b',
            result.stdout, re.MULTILINE):
        return 1
    return result.returncode


def history_runner_wrapper(root):
    directory = Path(tempfile.mkdtemp(prefix='tilo-history-gdb-'))
    wrapper = directory / 'cargo-runner'
    wrapper.write_text(
        '#!/bin/sh\nexec ' + shlex.quote(sys.executable) + ' -I '
        + shlex.quote(str(Path(__file__).resolve())) + ' history-gdb-runner "$@"\n',
        encoding='utf-8')
    wrapper.chmod(0o700)
    return wrapper


def execute(root, values, outcomes=None):
    # Cargo merges configuration from ancestors. The workflow helper checkout
    # must be a sibling, never a parent supplying another revision's settings.
    for parent in root.resolve().parents:
        if any((parent / '.cargo' / name).exists() for name in ('config', 'config.toml')):
            raise ValueError('source inherits an ancestor Cargo configuration')
    def read(args):
        return subprocess.check_output(args, cwd=root, text=True)
    actual = read(['git', 'rev-parse', 'HEAD']).strip()
    if actual != values['SOURCE_SHA']:
        raise ValueError('checked-out source differs from requested SHA')
    if values['OPERATION'] == 'inspect-native':
        print('TILO_NATIVE_ENVIRONMENT ' + json.dumps(
            {'source_sha': actual, **inspect_native_environment()}, sort_keys=True), flush=True)
        return 0
    metadata = json.loads(read(['cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1']))
    packages = [p for p in metadata['packages']
                if p['name'] == values['PACKAGE'] and p['id'] in metadata['workspace_members']]
    if len(packages) != 1:
        raise ValueError('package is not a unique workspace member')
    if values['OPERATION'] == 'check-tests':
        command = ['cargo', 'check', '--locked', '-p', values['PACKAGE'], '--tests']
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
    for case in cases:
        command = prefix + ['--', case, '--exact', '--nocapture']
        print(json.dumps({'source_sha': actual, 'command': command}), flush=True)
        # Every case uses this job's same target tree; Cargo reuses the target
        # compiled by --list. Keep Cargo's runtime library path setup intact.
        wrapper = None
        run_options = {'cwd': root, 'text': True, 'stdout': subprocess.PIPE}
        if values['OPERATION'] == HISTORY_BACKTRACE_OPERATION:
            wrapper = history_runner_wrapper(root)
            environment = os.environ.copy()
            environment[HISTORY_BACKTRACE_RUNNER] = str(wrapper)
            environment['TILO_HISTORY_SOURCE_ROOT'] = str(root.resolve())
            run_options['env'] = environment
        try:
            result = subprocess.run(command, **run_options)
        finally:
            if wrapper is not None:
                wrapper.unlink(missing_ok=True)
                if wrapper.parent.name.startswith('tilo-history-gdb-'):
                    wrapper.parent.rmdir()
        print(result.stdout, end='', flush=True)
        executed = bool(re.search(r'test result: (?:ok|FAILED)\. (?:1 passed; 0 failed|0 passed; 1 failed); 0 ignored;', result.stdout))
        passed = result.returncode == 0 and bool(re.search(
            r'test result: ok\. 1 passed; 0 failed; 0 ignored;', result.stdout))
        outcome = {'name': case, 'exit_code': result.returncode,
                   'executed': executed, 'passed': passed}
        outcomes.append(outcome)
        print('TILO_FOCUSED_CASE ' + json.dumps(outcome, sort_keys=True), flush=True)
        if not passed:
            code = code or result.returncode or 1
    return code


def main(arguments=None):
    arguments = sys.argv[1:] if arguments is None else arguments
    if arguments[:1] == ['history-gdb-runner']:
        try:
            return history_gdb_runner(arguments[1:],
                                      Path(os.environ['TILO_HISTORY_SOURCE_ROOT']).resolve())
        except (KeyError, OSError, ValueError) as error:
            print(str(error), file=sys.stderr)
            return 1
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('validate', 'run'))
    parser.add_argument('--root', type=Path, default=Path('source'))
    args = parser.parse_args(arguments)
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
        receipt['cases'] = []
        code = execute(args.root.resolve(), values, receipt['cases'])
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
