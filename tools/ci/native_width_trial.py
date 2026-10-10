#!/usr/bin/env python3
"""Diagnostic only: warm cache, identical native targets, then 8/6/6/8 builds.

Owns only a previously absent target directory in a disposable CI checkout.
No required workflow, compiler flags, cache keys or test filters are changed.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import threading
import time

PACKAGES = ('shell', 'tilo-ui')
CAP = 8 * 1024 * 1024


def counters(root, name):
    return {key: int(value) for key, value in
            (line.split() for line in (root / name).read_text().splitlines())}


def difference(before, after):
    if before.keys() != after.keys() or any(after[k] < before[k] for k in before):
        raise ValueError('Missing or reset cgroup counter')
    return {key: after[key] - value for key, value in before.items()}


def reset_owned_target(target):
    marker = target / '.native-width-owned'
    if target.is_symlink() or not marker.is_file() or marker.is_symlink():
        raise RuntimeError('Refusing cleanup of an unowned target')
    if any(path.is_symlink() for path in target.parents):
        raise RuntimeError('Refusing target with a symlink ancestor')
    shutil.rmtree(target)
    target.mkdir()
    marker.touch()


def binary_receipt(messages, target, package_ids):
    binaries = {}
    covered = set()
    finished = False
    for line in messages.splitlines():
        row = json.loads(line)
        if row.get('reason') == 'build-finished':
            finished = row.get('success') is True
        if row.get('reason') != 'compiler-artifact' or not row.get('executable'):
            continue
        path = Path(row['executable']).resolve(strict=True)
        if not path.is_relative_to(target.resolve()):
            raise RuntimeError('Compiled executable is outside the owned target')
        identity = (row['package_id'], row['target']['name'],
                    ','.join(row['target']['kind']), bool(row['profile']['test']))
        key = json.dumps(identity)
        if key in binaries:
            raise RuntimeError('Duplicate compiled executable identity')
        with path.open('rb') as binary:
            digest = hashlib.file_digest(binary, 'sha256').hexdigest()
        binaries[key] = digest
        if row['profile']['test'] and row['package_id'] in package_ids:
            covered.add(row['package_id'])
    if not finished or covered != set(package_ids) or not binaries:
        raise RuntimeError('Missing successful build or requested package tests')
    return binaries


def build_phase(jobs, output, target, package_ids, root):
    before = {name: counters(root, name) for name in ('cpu.stat', 'memory.events', 'memory.stat')}
    peaks = {'memory_bytes': 0, 'swap_bytes': 0}
    errors = []
    stop = threading.Event()

    def sample():
        try:
            while not stop.is_set():
                for field, file in [('memory_bytes', 'memory.current'), ('swap_bytes', 'memory.swap.current')]:
                    peaks[field] = max(peaks[field], int((root / file).read_text()))
                stop.wait(.25)
        except Exception as error:
            errors.append(type(error).__name__)

    sampler = threading.Thread(target=sample)
    sampler.start()
    started = time.monotonic()
    process = None
    chunks = []
    size = 0
    stderr_errors = []
    drainer = None
    try:
        with (output / f'build-{jobs}.log').open('wb') as log:
            process = subprocess.Popen(
                ['bash', 'tools/test-env.sh', 'cargo', 'test', '--locked', '--no-run', '--tests',
                 '-p', 'shell', '-p', 'tilo-ui', '--message-format=json-render-diagnostics'],
                env=dict(os.environ, CARGO_BUILD_JOBS=str(jobs)),
                stdout=subprocess.PIPE, stderr=subprocess.PIPE)

            def drain_errors():
                written = 0
                try:
                    while chunk := process.stderr.read(65536):
                        if written < 512 * 1024:
                            retained = chunk[:512 * 1024 - written]
                            log.write(retained)
                            written += len(retained)
                except Exception as error:
                    stderr_errors.append(type(error).__name__)

            drainer = threading.Thread(target=drain_errors)
            drainer.start()
            while chunk := process.stdout.read(65536):
                size += len(chunk)
                if size <= CAP:
                    chunks.append(chunk)
            status = process.wait()
            drainer.join()
    except BaseException:
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=30)
        raise
    finally:
        if drainer is not None:
            drainer.join(timeout=30)
            if drainer.is_alive():
                raise RuntimeError('Owned compiler stderr did not drain')
        if process is not None:
            for pipe in (process.stdout, process.stderr):
                if pipe is not None:
                    pipe.close()
        stop.set()
        sampler.join()
    events = difference(before['memory.events'], counters(root, 'memory.events'))
    cpu = difference(before['cpu.stat'], counters(root, 'cpu.stat'))
    receipt = {'jobs': jobs, 'wall_seconds': round(time.monotonic() - started, 3),
               'cpu': cpu, 'memory_events': events, 'peaks': peaks,
               'memory_stat_before': before['memory.stat'],
               'memory_stat_after': counters(root, 'memory.stat'),
               'sampling_errors': errors + stderr_errors, 'exit': status, 'metadata_bytes': size}
    (output / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print('NATIVE_WIDTH_TRIAL=' + json.dumps(receipt), flush=True)
    if status or size > CAP or errors or stderr_errors or peaks['swap_bytes'] or any(events.get(key, 0) for key in ('max', 'oom', 'oom_kill')):
        raise RuntimeError('Build or resource observation failed qualification')
    receipt['binaries'] = binary_receipt(b''.join(chunks).decode(), target, package_ids)
    (output / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    return receipt


def main():
    if os.environ.get('GITHUB_ACTIONS') != 'true' or os.environ.get('CARGO_TARGET_DIR'):
        raise RuntimeError('Run only in a fresh admitted CI checkout without target overrides')
    workspace = Path(os.environ['GITHUB_WORKSPACE']).resolve(strict=True)
    if Path.cwd().resolve() != workspace or (workspace / 'target').exists():
        raise RuntimeError('Expected a fresh disposable checkout')
    root = Path('/sys/fs/cgroup')
    quota, period = map(int, (root / 'cpu.max').read_text().split())
    if quota != 6 * period or int((root / 'memory.max').read_text()) != 22 * 1024**3:
        raise RuntimeError('Expected enforced 6 CPU / 22 GiB envelope')
    cpus = sorted(os.sched_getaffinity(0))
    if len(cpus) < 8:
        raise RuntimeError('Expected at least eight affinity CPUs')
    os.sched_setaffinity(0, cpus[:8])
    output = Path(os.environ['RUNNER_TEMP']) / 'native-width-trial'
    output.mkdir()
    if shutil.disk_usage(workspace).free < 30 * 1024**3:
        raise RuntimeError('Insufficient owned build scratch headroom')
    metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1'], text=True))
    package_ids = [row['id'] for row in metadata['packages'] if row['name'] in PACKAGES]
    if len(package_ids) != len(PACKAGES):
        raise RuntimeError('Missing requested packages')
    identity = {'source': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
                'rustc': subprocess.check_output(['rustc', '-Vv'], text=True).strip(),
                'quota': quota, 'period': period, 'memory_bytes': 22 * 1024**3,
                'affinity': sorted(os.sched_getaffinity(0)), 'packages': PACKAGES,
                'builder': os.environ['BUILDER_IMAGE']}
    (output / 'identity.json').write_text(json.dumps(identity, indent=2) + '\n')
    print('NATIVE_WIDTH_IDENTITY=' + json.dumps(identity), flush=True)
    target = workspace / 'target'
    target.mkdir()
    (target / '.native-width-owned').touch()
    receipts = []
    for index, jobs in enumerate((8, 8, 6, 6, 8)):
        phase = output / str(index)
        phase.mkdir()
        receipt = build_phase(jobs, phase, target, package_ids, root)
        if index:
            receipts.append(receipt)
            if receipt['binaries'] != receipts[0]['binaries']:
                raise RuntimeError('Native test/helper binary bytes changed across widths')
        if index != 4:
            reset_owned_target(target)
    # Execute the unchanged suite on the byte-identical final build. Existing
    # nextest profile still owns retries, test groups and GPU serialization.
    subprocess.run(['bash', 'tools/test-env.sh', 'cargo', 'nextest', 'run', '--locked',
                    '--profile', 'ci', '--tests', '-p', 'shell', '-p', 'tilo-ui'],
                   env=dict(os.environ, CARGO_BUILD_JOBS='8'), check=True)
    averages = {jobs: {field: sum(row[field] if field == 'wall_seconds' else row['cpu']['usage_usec']
                                 for row in receipts if row['jobs'] == jobs) / 2
                       for field in ('wall_seconds', 'cpu_usage_usec')} for jobs in (8, 6)}
    result = {'averages': averages, 'identical_binaries': True, 'full_tests': 'passed',
              'performance_qualified': all(averages[6][field] < averages[8][field] for field in averages[8])}
    (output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print('NATIVE_WIDTH_RESULT=' + json.dumps(result), flush=True)


if __name__ == '__main__':
    main()
