#!/usr/bin/env python3
"""Compare the source-content auditor with its version at a baseline commit.

Both audit this checkout's committed tree; they must agree on every inspected
blob and diagnostic. Prints each one's wall time, CPU time and the peak memory
of the cgroup it ran in. t's `source-blobs` check runs it on dispatch:

  t dispatch source-blobs --input order=candidate-first --input baseline=<sha>
"""
import argparse
import importlib.util
import json
import pathlib
import re
import subprocess
import time

SOURCE = 'tools/policy/check_source_content.py'
CGROUP = pathlib.Path('/sys/fs/cgroup')


def cgroup(name):
    """A file of the cgroup this runs in, or None where it can't be read."""
    try:
        return (CGROUP / name).read_text().strip()
    except OSError:
        return None


def cpu_usec():
    values = (cgroup('cpu.stat') or '').split()
    return int(values[values.index('usage_usec') + 1]) if 'usage_usec' in values else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--order', choices=('baseline-first', 'candidate-first'), default='baseline-first')
    parser.add_argument('--baseline', required=True, help='full commit holding the original auditor')
    args = parser.parse_args()
    if not re.fullmatch(r'[0-9a-f]{40}', args.baseline):
        raise SystemExit('baseline must be a full lowercase commit SHA')
    root = pathlib.Path.cwd()
    head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
    if subprocess.check_output(['git', 'status', '--porcelain', '--untracked-files=no']):
        raise SystemExit('the checkout has uncommitted changes; compare a committed tree')
    baseline = {'__name__': 'baseline_auditor', '__file__': str(root / SOURCE)}
    original = subprocess.check_output(['git', 'show', f'{args.baseline}:{SOURCE}'])
    exec(compile(original, SOURCE, 'exec'), baseline)
    spec = importlib.util.spec_from_file_location('candidate_auditor', root / SOURCE)
    candidate = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(candidate)
    auditors = {'baseline': baseline['audit'], 'candidate': candidate.audit}
    variants = ['baseline', 'candidate'] if args.order == 'baseline-first' else ['candidate', 'baseline']
    rows, results = [], []
    for variant in variants:
        started, before = time.monotonic(), cpu_usec()
        results.append(auditors[variant](root))
        rows.append({'variant': variant, 'wall_seconds': round(time.monotonic() - started, 6),
                     'cpu_seconds': round((cpu_usec() - before) / 1000000, 6),
                     'cumulative_job_peak_bytes': cgroup('memory.peak')})
    assert results[0] == results[1], 'auditors disagree on inspected blobs or diagnostics'
    assert not results[0][0] and results[0][1] > 0, 'source audit did not qualify'
    print(json.dumps({'source_sha': head, 'baseline_auditor_sha': args.baseline,
                      'order': args.order, 'inspected_blobs': results[0][1],
                      'cpu_max': cgroup('cpu.max'), 'memory_max': cgroup('memory.max'),
                      'measurements': rows}, indent=2))


if __name__ == '__main__':
    main()
