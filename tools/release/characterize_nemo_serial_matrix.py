#!/usr/bin/env python3
"""Replay exposed development clips against the isolated serial scheduler build.

No downloads, holdout decoding, device access or installed runtime changes.
Uses the actual unchanged 1 GiB address-space limit and a 960 MiB live guard.
"""
from pathlib import Path
import hashlib
import json
import os
import subprocess

root = Path('/tmp/tilo-dictation')
out = root / 'target/asr-nemo-serial-scheduler-v1'
receipts = json.loads((out / 'build-receipts.json').read_text())
expected = '85fe65d8514aa1dfc5b233ecb16ab6eca384466d1964515ff8bf4732b619d0ee'
if receipts['library_sha256'] != expected:
    raise ValueError('exact characterized native library required')
if hashlib.sha256((out / 'compact-bin/libnemo_speech_asr.so').read_bytes()).hexdigest() != expected:
    raise ValueError('native library no longer matches its receipt')
shared = {'libggml-base.so': '2317e1ea25f4942dc886f437e1a4a0dc5c1dc90a88f12cbbbc447aaec6a3dfca', 'libggml-cpu.so': 'c09b6188fe7eb70da88c32513097120c83e2ead6413756bf24367fb5a2f76be0', 'libggml.so': '9bf386050684fc3039b4327529ef93f6b3cb430bbf3c9951bd8a71dea0c397f6', 'libnemo_speech_asr.so': '85fe65d8514aa1dfc5b233ecb16ab6eca384466d1964515ff8bf4732b619d0ee', 'libnemo_speech_asr_c.so': 'bc22ed3b6f212b88a86d06dcabb5f2d256af4b763f760f540827b5dc140379f0'}
for name, digest in shared.items():
    if hashlib.sha256((out / 'compact-bin' / name).read_bytes()).hexdigest() != digest:
        raise ValueError('experimental shared library no longer matches its receipt: ' + name)
probe = (out / 'probe-compact-2g.py').read_text()
if (probe.count('2147483648') != 3 or probe.count('1984 * 1024') != 1
        or probe.count('1984*1024*1024') != 1):
    raise ValueError('exact separate review limit literals changed')
probe = (probe.replace('2147483648', '1073741824')
    .replace('1984 * 1024', '960 * 1024')
    .replace('1984*1024*1024', '960*1024*1024')
    .replace('CHARACTERIZATION 2GiB', 'CHARACTERIZATION 1GiB'))
(out / 'probe-compact-1g.py').write_text(probe)
rows = [('development-read-' + lang + '-0', 2)
    for lang in ['en_us', 'de_de', 'fr_fr', 'es_419', 'ar_eg', 'cmn_hans_cn']]
rows.append(('development-read-fr_fr-0', 1))
for case, cpus in rows:
    available = int(next(line.split()[1]
        for line in Path('/proc/meminfo').read_text().splitlines()
        if line.startswith('MemAvailable:')))
    pressure = float(Path('/proc/pressure/memory').read_text()
        .splitlines()[0].split()[1].split('=')[1])
    print('SERIAL_SCHEDULER_MATRIX_HEADROOM ' + json.dumps(dict(case=case,
        available_kib=available, pressure_some_avg10=pressure,
        actual_cap_bytes=1073741824)), flush=True)
    if available < 8 * 1024 * 1024 or pressure > 5:
        raise RuntimeError('stop: pressure/headroom')
    subprocess.run(['python3', str(out / 'probe-compact-1g.py'), '--model',
        'nemotron-q8', '--case', case, '--mode', 'paced', '--cpus', str(cpus),
        '--allocator', 'arena-1', '--nemo-api', 'native'],
        env=dict(os.environ, PYTHONPATH=str(root / 'tools/release')), check=True)
