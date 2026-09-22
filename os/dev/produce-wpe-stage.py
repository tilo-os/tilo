#!/usr/bin/env python3
"""Measured manual development WPE producer inside a disposable native CI guest.

Capacity thresholds are experiment guards, not estimates of WebKit requirements.
Only public receipts leave /evidence. Raw child logs and registry auth stay private.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import stat
import subprocess
import tempfile
import time

REPO = Path(__file__).resolve().parents[2]
EVIDENCE = Path('/evidence')
GIB = 1024 ** 3
START_FREE = 30 * GIB
ABORT_FREE = 8 * GIB
DEADLINE_SECONDS = 155 * 60


def validate(args):
    if args.architecture not in ('x86_64', 'aarch64') or not re.fullmatch('[0-9a-f]{40}', args.revision):
        raise ValueError('native architecture and committed source required')
    if not args.branch.startswith('refs/heads/codex/'):
        raise ValueError('development feature branch required')
    for field in ('run_id', 'attempt'):
        if not re.fullmatch('[1-9][0-9]*', getattr(args, field)):
            raise ValueError('CI run identity required')
    for field, repository in (('wpe', 'tilo-wpe-buildroot'), ('tools_builder', 'tilo-builder')):
        if not re.fullmatch('code\\.podesta\\.ai/qos/' + repository + '@sha256:[0-9a-f]{64}', getattr(args, field)):
            raise ValueError('reviewed immutable native image required')


def capacity_ok(free, starting=False):
    return free >= (START_FREE if starting else ABORT_FREE)


def memory():
    result = {}
    for field in ('memory.max', 'memory.current', 'memory.peak', 'memory.events'):
        path = Path('/sys/fs/cgroup') / field
        if path.is_file():
            result[field] = path.read_text().strip()
    return result


def capacity():
    return {path: shutil.disk_usage(path).free for path in ('/work', '/var/lib/containers')}


def write(name, value):
    (EVIDENCE / name).write_text(json.dumps(value, sort_keys=True, indent=2) + '\n')


def stop(process):
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=10)


def compiler_diagnostics(path):
    """Retain bounded, recognized compiler records, never registry tool errors."""
    keep = []
    timings = []
    # Build logs may be large. Stream and retain only a bounded diagnostic tail.
    spec = importlib.util.spec_from_file_location('wpe_diagnostics', REPO / 'os/dev/prepare-wpe-stage.py')
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    with path.open('rb') as stream:
        for text in helper.bounded_lines(stream):
            timing = re.search(r'TILO_BUILD_TIMING phase=wpe elapsed_seconds=([0-9]+) exit_code=([0-9]+)', text)
            if timing:
                timings.append({'phase': 'wpe', 'elapsed_seconds': int(timing[1]), 'exit_code': int(timing[2])})
                timings = timings[-8:]
            if (re.search(r'[^\s]+\.(?:c|cc|cpp|cxx|h|hpp):[0-9]+(?::[0-9]+)?: (?:fatal )?error:', text)
                    or re.search(r'meson\.build:[0-9]+:[0-9]+: ERROR:', text)
                    or re.search(r'(?:^|/)ld(?:\.lld|\.bfd)?:.*(?:undefined reference|cannot find)', text)
                    or re.search(r'\.(?:c|cc|cpp|cxx):.*undefined reference', text)
                    or re.search(r'^(?:/[^\s]+/)?(?:gcc|g\+\+|cc|c\+\+): fatal error:', text)
                    or text.startswith(('ninja: error:', 'ninja: build stopped:', 'FAILED: wpewebkit compile',
                                        'TILO_WPE_COMPILER_DIAGNOSTIC ', 'collect2: error: ld returned'))):
                keep.append(text)
                keep = keep[-80:]
    return {'diagnostic_tail': keep, 'compiler_timings': timings,
            'raw_registry_and_tool_stderr_retained': False}


def phase(name, argv, environment, private, deadline, measurements):
    started = time.monotonic()
    record = {'phase': name, 'status': 'running', 'samples': 0,
              'minimum_free_bytes': min(capacity().values())}
    measurements.append(record)
    # Neither stdout nor stderr is exported: auth failures can contain private
    # registry details. Public progress contains only measured resource values.
    output = private / (name + '.stdout')
    error_output = private / (name + '.stderr')
    with output.open('wb') as stdout, error_output.open('wb') as stderr:
        process = subprocess.Popen(argv, cwd=REPO, env=environment, stdout=stdout, stderr=stderr,
                                   start_new_session=True)
        try:
            while process.poll() is None:
                free = min(capacity().values())
                record['samples'] += 1
                record['minimum_free_bytes'] = min(record['minimum_free_bytes'], free)
                record['elapsed_seconds'] = round(time.monotonic() - started, 3)
                record['memory'] = memory()
                write('phases.json', measurements)
                print(json.dumps({'phase': name, 'elapsed_seconds': record['elapsed_seconds'],
                                  'free_bytes': free}), flush=True)
                if not capacity_ok(free):
                    record['status'] = 'disk-reserve-abort'
                    raise ValueError('disk reserve reached')
                if time.monotonic() >= deadline:
                    record['status'] = 'bounded-duration-abort'
                    raise ValueError('producer duration guard reached')
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    pass
            record['exit_code'] = process.returncode
            record['status'] = 'success' if process.returncode == 0 else 'command-failed'
            if process.returncode:
                raise ValueError('producer phase failed')
        finally:
            stop(process)
            record['elapsed_seconds'] = round(time.monotonic() - started, 3)
            record['memory'] = memory()
            write('phases.json', measurements)
            if name == 'build-export':
                stderr.flush()
                write('compiler-diagnostics.json', compiler_diagnostics(error_output))
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for field in ('architecture', 'revision', 'branch', 'run-id', 'attempt', 'wpe', 'tools-builder'):
        parser.add_argument('--' + field, required=True)
    args = parser.parse_args()
    def interrupted(_signal, _frame):
        raise InterruptedError('producer interrupted')
    signal.signal(signal.SIGTERM, interrupted)
    measurements = []
    started = time.monotonic()
    EVIDENCE.mkdir(mode=0o700, exist_ok=True)
    result = {'schema': 1, 'purpose': 'development-wpe-producer', 'status': 'failed',
              'real_wpe_build_attempted': False, 'production_signed': False}
    try:
        validate(args)
        if os.getuid() != 0 or platform.machine() != args.architecture or not any(
                Path(path).exists() for path in ('/.dockerenv', '/run/.containerenv')):
            raise ValueError('disposable native root container required')
        spec = importlib.util.spec_from_file_location('wpe_artifact', REPO / 'os/dev/wpe-stage-artifact.py')
        artifact = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(artifact)
        artifact.verify_source(REPO, args.revision)
        auth = Path('/registry-auth.json')
        info = auth.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o077 or info.st_size > 65536:
            raise ValueError('private registry authentication required')
        free, budget = capacity(), memory()
        write('admission.json', {'free_bytes': free, 'memory': budget, 'required_free_bytes': START_FREE,
                                'abort_free_bytes': ABORT_FREE, 'jobs': 6, 'duration_guard_seconds': DEADLINE_SECONDS})
        maximum = budget.get('memory.max', '')
        if not maximum.isdecimal() or int(maximum) != 23 * GIB:
            raise ValueError('explicit 23 GiB producer cgroup required')
        if not capacity_ok(min(free.values()), starting=True):
            raise ValueError('starting storage reserve not met')
        inputs = vars(args) | {'jobs': 6, 'epoch': '0', 'runner_class': 'tilo-package',
                              'disk_capacity_proven': False}
        write('inputs.json', inputs)
        pins = {}
        for variable, filename in (('base', 'base-snapshot.env'), ('rpms', 'rpms-snapshot.env'),
                                   ('kernel', 'kernel-snapshot.env')):
            key = {'base': 'BASE_REF', 'rpms': 'RPMS_REF', 'kernel': 'KERNEL_RPMS_REF'}[variable]
            values = re.findall('^' + key + r'=(code\.podesta\.ai/qos/[a-z0-9-]+@sha256:[0-9a-f]{64})$',
                                (REPO / 'os' / filename).read_text(), re.MULTILINE)
            if len(values) != 1:
                raise ValueError('immutable dependency parse inputs required')
            pins[variable] = values[0]
        script = artifact.read_bounded(REPO / 'os/dev/build-wpe.sh', 2 * 1024 * 1024).decode()
        archives = []
        for variable in ('LIBWPE', 'FDO', 'WPEWK'):
            names = re.findall('^' + variable + r'=([a-z0-9.-]+\.tar\.xz)$', script, re.MULTILINE)
            if len(names) != 1:
                raise ValueError('one WPE archive pin required')
            archives.extend(names)
        environment = {'PATH': os.defpath, 'HOME': '/root', 'LANG': 'C.UTF-8',
                       'REGISTRY_AUTH_FILE': str(auth)}
        with tempfile.TemporaryDirectory(prefix='tilo-wpe-producer-', dir='/work') as temporary:
            private = Path(temporary)
            environment['TMPDIR'] = str(private)
            deadline = started + DEADLINE_SECONDS
            phase('assets', ['bash', 'os/dev/cache-github-assets.sh', *archives], environment,
                  private, deadline, measurements)
            argv = ['python3', 'os/dev/wpe-stage-artifact.py', 'build-export',
                    '--architecture', args.architecture, '--epoch', '0', '--jobs', '6',
                    '--repo', str(REPO), '--assets', str(REPO / 'os/external-assets/cache'),
                    '--wpe', args.wpe, '--revision', args.revision, '--run-id', args.run_id,
                    '--attempt', args.attempt, '--cache',
                    'code.podesta.ai/qos/tilo-development-wpe-cache-' + args.architecture]
            for key, value in pins.items():
                argv.extend(['--' + key, value])
            result['real_wpe_build_attempted'] = True
            output = phase('build-export', argv, environment, private, deadline, measurements)
            receipt = artifact.strict_json(artifact.read_bounded(output, 65536))
            expected = artifact.dependency_inputs(REPO, REPO / 'os/external-assets/cache',
                                                  args.architecture, '0', args.wpe)
            artifact.validate_receipt(receipt, expected)
            write('wpe-stage.json', receipt)
            result['status'] = 'success'
    except (OSError, ValueError, TypeError, KeyError, subprocess.SubprocessError):
        result['failure_category'] = 'validation-capacity-or-producer-failure'
    finally:
        result['elapsed_seconds'] = round(time.monotonic() - started, 3)
        result['phases'] = measurements
        result['memory_final'] = memory()
        write('result.json', result)
    return 0 if result['status'] == 'success' else 1


if __name__ == '__main__':
    raise SystemExit(main())
