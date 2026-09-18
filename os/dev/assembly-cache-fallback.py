#!/usr/bin/env python3
"""Retry one optional cache-export failure using the same local layer store."""
import os
import json
from pathlib import Path
import re
import shlex
import subprocess
import sys


def without_export(environment):
    env = environment.copy()
    args = shlex.split(env.get('TILO_BUILD_ARGS', ''))
    repository = r'code\.podesta\.ai/qos/tilo-development-assembly-cache-(?:aarch64|x86_64)-v1'
    positions = [i for i, arg in enumerate(args) if arg == '--cache-to']
    if (len(positions) != 1 or positions[0] + 1 >= len(args)
            or not re.fullmatch(repository, args[positions[0] + 1])
            or '--layers=true' not in args or '--no-cache' in args
            or env.get('TILO_RAW_ROOTFS_ONLY') != '1'
            or env.get('TILO_PUBLIC_TRUST_MODE') != 'development'):
        return None
    index = positions[0]
    del args[index:index + 2]
    # build-image.sh splits this variable on whitespace, not shell syntax.
    if any(any(c.isspace() for c in arg) for arg in args):
        return None
    env['TILO_BUILD_ARGS'] = ' '.join(args)
    return env


def export_failure(code, tail):
    # Exact Podman failure boundary; never retry compilation/RUN, cache import,
    # capacity, signal or arbitrary registry failures as successful builds.
    errors = [line for line in tail.decode('utf-8', errors='replace').splitlines()
              if line.startswith('Error:')]
    return (code == 125 and bool(errors) and
            errors[-1].startswith('Error: failed pushing cache to ') and
            'writing blob:' in errors[-1] and
            bool(re.search(r'HTTP status: 5[0-9]{2}(?:\s|$)', errors[-1])))


def attempt(command, environment):
    tail = b''
    with subprocess.Popen(command, env=environment, stdout=subprocess.PIPE,
                          stderr=subprocess.STDOUT) as child:
        while True:
            chunk = child.stdout.read1(65536)
            if not chunk:
                break
            sys.stdout.buffer.write(chunk)
            sys.stdout.buffer.flush()
            tail = (tail + chunk)[-65536:]
        return child.wait(), tail


def execute(command, environment, runner=attempt, record=lambda value: None):
    code, tail = runner(command, environment)
    fallback = without_export(environment)
    if fallback is not None and export_failure(code, tail):
        evidence = {'schema': 1, 'optional_export_failed': True, 'first_exit_code': code,
                    'retry_without_export': True, 'local_layers_retained': True,
                    'cache_hits_verified': False, 'retry_exit_code': None}
        record(evidence)
        print('Optional assembly cache upload failed with HTTP 5xx; retrying once without export using unchanged local layers and cache imports.', flush=True)
        code, _ = runner(command, fallback)
        evidence['retry_exit_code'] = code
        record(evidence)
    return code


if __name__ == '__main__':
    def record(value):
        Path('/evidence/cache-export-fallback.json').write_text(json.dumps(value, sort_keys=True) + '\n')
    raise SystemExit(execute(['bash', 'os/dev/build-image.sh'], os.environ, record=record))
