#!/usr/bin/env python3
"""Replay predeclared exposed stress cases in the isolated 1 GiB prototype.

Synthetic noise/assembled transitions do not qualify natural noise or switching.
No downloads, holdout decoding, installed model or device access.
"""
from pathlib import Path
import argparse
import hashlib
import json
import os
import subprocess

root = Path('/tmp/tilo-dictation')
out = root / 'target/asr-nemo-serial-scheduler-v1'
ledger = json.loads((root / 'docs/architecture/dictation-streaming-optimization.json').read_text())
shared = ledger['source_build_receipts']['voice-nemo-serial-scheduler.log']['shared_library_sha256']
for name, digest in shared.items():
    if Path(name).name != name or len(digest) != 64:
        raise ValueError('plain shared-library receipt required')
    if hashlib.sha256((out / 'compact-bin' / name).read_bytes()).hexdigest() != digest:
        raise ValueError('experimental shared-library receipt mismatch: ' + name)
probe = out / 'probe-compact-1g.py'
source = probe.read_text()
if '2147483648' in source or source.count('1073741824') != 3:
    raise ValueError('actual original 1 GiB probe required')
rows = ['development-synthetic-noise-' + str(snr) + 'db-' + lang + '-0'
    for lang in ['en_us', 'de_de', 'fr_fr', 'es_419', 'ar_eg', 'cmn_hans_cn']
    for snr in [20, 10]]
rows += ['development-assembled-transition-en_us-0+de_de-0',
    'development-assembled-transition-fr_fr-0+es_419-0']
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--resume-from', choices=rows)
args = parser.parse_args()
if args.resume_from:
    rows = rows[rows.index(args.resume_from):]
for case in rows:
    available = int(next(line.split()[1]
        for line in Path('/proc/meminfo').read_text().splitlines()
        if line.startswith('MemAvailable:')))
    pressure = float(Path('/proc/pressure/memory').read_text()
        .splitlines()[0].split()[1].split('=')[1])
    print('SERIAL_STRESS_HEADROOM ' + json.dumps(dict(case=case,
        available_kib=available, pressure_some_avg10=pressure,
        actual_cap_bytes=1073741824)), flush=True)
    if available < 8 * 1024 * 1024 or pressure > 5:
        raise RuntimeError('stop: pressure/headroom')
    subprocess.run(['python3', str(probe), '--model', 'nemotron-q8',
        '--case', case, '--mode', 'paced', '--cpus', '2', '--allocator',
        'arena-1', '--nemo-api', 'native'],
        env=dict(os.environ, PYTHONPATH=str(root / 'tools/release')), check=True)
