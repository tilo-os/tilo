#!/usr/bin/env python3
"""Observe the actual speech-worker W^X policy, without a recognizer or model.

Use an existing admitted transient scope. The opt-in worker runs a fixed x86-64
instruction fixture only after removing write permission, then requires both
mprotect and the exact failed V8 pkey_mprotect shape to return EPERM. This is a
policy compatibility check, not recognition, performance or runtime admission.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path

import probe_asr_candidate as probe
import qualify_whistle_cli_scope as scope

PROJECT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runtime-dir', type=Path, required=True)
    args = parser.parse_args()
    if os.geteuid() == 0 or os.geteuid() != os.getuid():
        parser.error('unprivileged source-only qualification required')
    cg = scope.admitted_scope()
    cpus = sorted(os.sched_getaffinity(0))[:2]
    if len(cpus) != 2:
        raise RuntimeError('two admitted CPUs required')
    root = args.runtime_dir.resolve(strict=True)
    receipt = json.loads((root / 'receipt.json').read_text())
    executable = root / 'policy-review'
    if (executable.is_symlink() or not executable.is_file()
            or executable.stat().st_uid != os.getuid()):
        raise RuntimeError('owned regular qualification executable required')
    probe.receipt(executable, receipt['binary']['bytes'], receipt['binary']['sha256'])
    for name, digest in receipt['source'].items():
        path = PROJECT / name
        if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            raise RuntimeError('measured policy source differs from checkout')
    def events():
        return dict((line.split()[0], int(line.split()[1]))
                    for line in (cg / 'memory.events').read_text().splitlines())
    before = events()
    command = ['bwrap', '--unshare-all', '--die-with-parent', '--ro-bind', '/usr', '/usr',
               '--symlink', 'usr/lib64', '/lib64', '--symlink', 'usr/lib', '/lib',
               '--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp', '--dir', '/review',
               '--ro-bind', str(executable), '/review/policy-review', '--clearenv',
               '--setenv', 'LANG', 'C.UTF-8', '--chdir', '/', '/usr/bin/time', '-f',
               'ASR_RESOURCE {"user":%U,"system":%S}', '/review/policy-review', '--policy-probe']
    identity = executable.stat()
    result = probe.observe(command, cpus, (identity.st_dev, identity.st_ino),
                           max_threads=2, capture_lines=True)
    after = events()
    result.update(source_receipt=receipt, scope_before=before, scope_after=after,
                  scope_peak_bytes=int((cg / 'memory.peak').read_text()),
                  not_a_recognition_or_latency_measurement=True)
    print('SPEECH_POLICY_SCOPE_RESULT ' + json.dumps(result), flush=True)
    if (result['exit'] or result['guarded'] or result['memory_peak'].get('Threads', 0) != 1
            or any(before[key] != after[key] for key in ('high', 'max', 'oom', 'oom_kill'))):
        raise RuntimeError('actual policy compatibility observation failed')
    rows = [json.loads(line.split(' ', 1)[1]) for line in result['stdout'].splitlines()
            if line.startswith('SPEECH_POLICY_PROBE ')]
    if rows != [{'rw_to_rx_executes': True, 'mprotect_wx_eperm': True,
                 'exact_v8_pkey_wx_eperm': True, 'production_confinement': True}]:
        raise RuntimeError('exact actual kernel observations required')


if __name__ == '__main__':
    main()
