#!/usr/bin/env python3
"""Inject one public FFT allocation failure; assert it remains inside the ABI catch.

This diagnostic preload is compiled in a temporary directory. It is never
installed or loaded by the service. Requires the same isolated root builder as
probe_speech_memory.py; the decoder still runs as nobody with its normal sandbox.
"""
import argparse
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

from probe_speech_memory import MODELS, observe


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--benchmark', type=Path, required=True)
    parser.add_argument('--expect', choices=('legacy-abort', 'typed-failure'),
                        default='typed-failure')
    args = parser.parse_args()
    if os.geteuid() != 0 or args.manifest.stat().st_size > 1024 * 1024:
        parser.error('run in the isolated root builder with a bounded public manifest')
    manifest = json.loads(args.manifest.read_text())
    if (manifest.get('dataset') != 'google/fleurs'
            or manifest.get('revision') != '70bb2e84b976b7e960aa89f1c648e09c59f894dd'
            or len(manifest.get('cases', [])) != 33):
        parser.error('unexpected public corpus receipt')
    model = manifest['models']['small-q5_1']
    if model['sha256'] != MODELS['small-q5_1']:
        parser.error('unexpected model receipt')
    receipts = [model, *manifest['cases']]
    source = args.manifest.resolve().parent
    for receipt in receipts:
        name = receipt['file']
        if Path(name).name != name:
            parser.error('receipt paths must be plain filenames')
        file = source / name
        if (file.stat().st_size != receipt['size']
                or hashlib.sha256(file.read_bytes()).hexdigest() != receipt['sha256']):
            parser.error('public fixture digest mismatch')
    with tempfile.TemporaryDirectory(prefix='tilo-public-fft-fault-') as directory:
        root = Path(directory)
        root.chmod(0o755)
        library = root / 'fft-fault.so'
        subprocess.run(['c++', '-g', '-O2', '-shared', '-fPIC', '-o', str(library),
                        str(Path(__file__).parent / 'fixtures/speech_fft_fault.cpp'),
                        '-ldl'], check=True)
        fixture = root / 'corpus'
        fixture.mkdir(mode=0o700)
        for name in ['manifest.json', *[r['file'] for r in receipts]]:
            target = fixture / name
            shutil.copyfile(source / name, target)
            target.chmod(0o600)
            os.chown(target, 65534, 65534)
        os.chown(fixture, 65534, 65534)
        command = [str(args.benchmark.resolve()), '--manifest',
                   str(fixture / 'manifest.json'), '--model', 'small-q5_1',
                   '--single-case', 'ar_eg-1']
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            observe(command, {'LANG': 'C.UTF-8', 'LD_PRELOAD': str(library)},
                    {'user': 65534, 'group': 65534, 'extra_groups': []}, 0,
                    model['sha256'], 'ar_eg-1', 'controlled-FFT-allocation-failure')
        raw = output.getvalue()
        print(raw, end='', flush=True)
        rows = [json.loads(line.split(' ', 1)[1]) for line in raw.splitlines()
                if line.startswith('SPEECH_MEMORY_PROBE ')]
        expected = -6 if args.expect == 'legacy-abort' else 1
        if (raw.count('PUBLIC_PROBE_INJECTED_FFT_ALLOCATION_FAILURE') != 1
                or len(rows) != 1 or rows[0]['exit'] != expected
                or rows[0]['last_phase'] != 'draft-fixed-language'
                or (expected == 1 and 'Error: "draft recognition failed"' not in raw)):
            raise SystemExit('allocation failure was not observed and contained as expected')
        print('PUBLIC_FFT_FAILURE_REGRESSION_PASS', args.expect)


if __name__ == '__main__':
    main()
