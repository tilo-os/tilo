#!/usr/bin/env python3
"""Bounded proof of complete SDK probes on identical WASM across native hosts."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

NAMES = {'tilo_browser_runtime_bg.wasm', 'tilo_browser_compute_bg.wasm'}


def wasm_inputs(bundle):
    files = sorted(bundle.rglob('*.wasm'))
    if len(files) != 2 or {path.name for path in files} != NAMES:
        raise RuntimeError('Both exact desktop WASM modules must be observed')
    result = {}
    for path in files:
        if path.is_symlink() or not path.resolve().is_relative_to(bundle.resolve()):
            raise RuntimeError('WASM input escaped the owned bundle')
        digest = hashlib.sha256()
        with path.open('rb') as handle:
            if handle.read(8) != b'\0asm\x01\0\0\0':
                raise RuntimeError('Invalid WASM artifact')
            handle.seek(0)
            while chunk := handle.read(1024 * 1024):
                digest.update(chunk)
        result[str(path.relative_to(bundle))] = {'sha256': digest.hexdigest(), 'bytes': path.stat().st_size}
    return result


def equivalent(rows):
    if len(rows) != 2 or {row['arch'] for row in rows} != {'x86_64', 'aarch64'}:
        raise RuntimeError('Both native host receipts are required')
    for row in rows:
        if not row['full_probes_passed'] or row['chrome_version'].split()[-1] != '155.0.8059.39':
            raise RuntimeError('Complete probes on the exact browser version did not pass')
    for key in ('source', 'builder', 'wasm'):
        if rows[0][key] != rows[1][key]:
            raise RuntimeError(f'Native hosts differ in {key}')
    return {'source': rows[0]['source'], 'builder': rows[0]['builder'],
            'identical_wasm': rows[0]['wasm'], 'full_probes_passed': True}


def main():
    if os.environ.get('GITHUB_ACTIONS') != 'true':
        raise RuntimeError('Use the existing admitted CI workflow')
    scratch = Path(os.environ['RUNNER_TEMP'])
    mode = sys.argv[1]
    if mode == 'compare':
        files = sorted((scratch / 'sdk-web-host-receipts').glob('*/sdk-web-host-receipt.json'))
        result = equivalent([json.loads(path.read_text()) for path in files])
        print('SDK_WEB_HOST_COMPARISON=' + json.dumps(result), flush=True)
        return
    bundle = Path('target/tilo-app/web/calculator/bundle')
    output = scratch / 'sdk-web-host-receipt.json'
    cgroup = Path('/sys/fs/cgroup')
    events = dict(line.split() for line in (cgroup / 'memory.events').read_text().splitlines())
    if any(int(events[key]) for key in ('oom', 'oom_kill')) or (cgroup / 'memory.swap.max').read_text().strip() != '0':
        raise RuntimeError('OOM or nonzero swap allowance disqualified the host')
    if mode == 'before':
        receipt = {'source': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
                   'builder': os.environ['BUILDER_IMAGE'], 'arch': os.environ['SDK_HOST_ARCH'],
                   'cpu_max': (cgroup / 'cpu.max').read_text().strip(),
                   'memory_max': int((cgroup / 'memory.max').read_text()),
                   'wasm': wasm_inputs(bundle), 'full_probes_passed': False}
        with output.open('x') as handle:
            json.dump(receipt, handle)
    elif mode == 'after':
        receipt = json.loads(output.read_text())
        if receipt['wasm'] != wasm_inputs(bundle):
            raise RuntimeError('Desktop WASM changed during the full probes')
        receipt['chrome_version'] = subprocess.check_output([os.environ['CHROME'], '--version'], text=True).strip()
        if receipt['chrome_version'].split()[-1] != '155.0.8059.39':
            raise RuntimeError('Unexpected browser version')
        receipt['memory_events'] = {key: int(value) for key, value in events.items()}
        receipt['full_probes_passed'] = True
        output.write_text(json.dumps(receipt, indent=2) + '\n')
    else:
        raise RuntimeError('Unknown proof phase')
    print('SDK_WEB_HOST_RECEIPT=' + json.dumps(receipt), flush=True)


if __name__ == '__main__':
    main()
