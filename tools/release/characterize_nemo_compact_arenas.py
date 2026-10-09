#!/usr/bin/env python3
"""Remote review-only bounded metadata reservation experiment.

Neither recognizer option is public or deployed. Preserve graph-capacity guards,
restore pinned source/build artifacts, and measure only exposed public cases.
Requires existing reviewed native fixtures and graph8192 receipts. No downloads.
"""
from pathlib import Path
import hashlib
import json
import os
import shutil
import subprocess

root = Path('/tmp/tilo-dictation')
source = root/'target/nemo-asr-review-v1/source'
build = source/'build/cpu-asr'
out = root/'target/asr-nemo-compact-arenas-v1'
if out.exists():
    raise ValueError('never overwrite an existing optimization receipt')
available = int(next(l.split()[1] for l in Path('/proc/meminfo').read_text().splitlines() if l.startswith('MemAvailable:')))
pressure = float(Path('/proc/pressure/memory').read_text().splitlines()[0].split()[1].split('=')[1])
print('COMPACT_ARENAS_HEADROOM '+json.dumps(dict(available_kib=available, pressure_some_avg10=pressure, separate_cap_bytes=2147483648, production_changed=False)), flush=True)
if available < 8*1024*1024 or pressure > 5 or shutil.disk_usage(build).free < 4*1024**3:
    raise RuntimeError('stop: memory pressure or memory/disk headroom')
session = source/'src/runtime/ggml/session.cpp'
header = source/'src/runtime/ggml/runtime.h'
original = {session: session.read_bytes(), header: header.read_bytes()}
if hashlib.sha256(original[session]).hexdigest() != 'b37a3a1b5810d9034a847aa0923d5ae7063ec70f8f43080fec36c01db6e6d81c':
    raise ValueError('exact original scheduler required')
if hashlib.sha256(original[header]).hexdigest() != '8b16754a48bc69a9d0f9d2bc29913e9cf4a7e9b5321bd118780093010c5c0f25':
    raise ValueError('exact original metadata arenas required')
if original[header].count(b'64 * 1024 * 1024') != 2:
    raise ValueError('pinned default metadata reservations changed')
out.mkdir(mode=0o700)
shutil.copytree(build/'bin', out/'original-bin', symlinks=True)
artifacts = [*build.rglob('*.a'), *build.rglob('*.o')]
for path in artifacts:
    saved = out/'original-intermediates'/path.relative_to(build)
    saved.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(path, saved)
patched = {
    session: original[session].replace(b'static constexpr size_t kSchedGraphSize = 65536;', b'static constexpr size_t kSchedGraphSize = 4096;'),
    header: original[header].replace(b'64 * 1024 * 1024', b'4 * 1024 * 1024'),
}
try:
    for path, data in patched.items():
        path.write_bytes(data)
        (out/(path.name+'.experimental')).write_bytes(data)
        (out/(path.name+'.original')).write_bytes(original[path])
    subprocess.run(['cmake', '--build', str(build), '--target', 'nemo_speech_asr', '--parallel', '2'], check=True)
    shutil.copytree(build/'bin', out/'compact-bin', symlinks=True)
finally:
    for path, data in original.items():
        path.write_bytes(data)
    for path in (out/'original-bin').iterdir():
        target = build/'bin'/path.name
        if target.is_symlink():
            target.unlink()
        if path.is_symlink():
            target.symlink_to(os.readlink(path))
        elif path.is_file():
            shutil.copy2(path, target)
    for path in artifacts:
        shutil.copy2(out/'original-intermediates'/path.relative_to(build), path)
    for path, data in original.items():
        if path.read_bytes() != data:
            raise RuntimeError('pinned source restoration failed')
    for path in (out/'original-bin').iterdir():
        if path.is_file() and not path.is_symlink() and path.read_bytes() != (build/'bin'/path.name).read_bytes():
            raise RuntimeError('pinned library restoration failed')
receipts = dict(graph_capacity=4096, default_metadata_arena_bytes=4194304,
    public_option=False, deployed=False,
    source_original_sha256={p.name: hashlib.sha256(b).hexdigest() for p,b in original.items()},
    source_experimental_sha256={p.name: hashlib.sha256(b).hexdigest() for p,b in patched.items()},
    library_sha256=hashlib.sha256((out/'compact-bin/libnemo_speech_asr.so').read_bytes()).hexdigest(),
    shared_library_sha256={p.name: hashlib.sha256(p.read_bytes()).hexdigest()
        for p in sorted((out/'compact-bin').glob('*.so')) if not p.is_symlink()})
(out/'build-receipts.json').write_text(json.dumps(receipts, indent=2)+'\n')
print('COMPACT_ARENAS_BUILD '+json.dumps(receipts), flush=True)
probe = (root/'target/asr-nemo-compare-slots1-v1/probe-graph8192-2g.py').read_text()
needle = "ROOT/'asr-nemo-scheduler-characterization-v1/graph8192-bin'"
if probe.count(needle) != 1:
    raise ValueError('single-stream library namespace changed')
probe = probe.replace(needle, "ROOT/'asr-nemo-compact-arenas-v1/compact-bin'").replace('Nemo internal graph8192 prototype', 'Nemo internal graph4096/metadata4MiB prototype')
(out/'probe-compact-2g.py').write_text(probe)
available = int(next(l.split()[1] for l in Path('/proc/meminfo').read_text().splitlines() if l.startswith('MemAvailable:')))
pressure = float(Path('/proc/pressure/memory').read_text().splitlines()[0].split()[1].split('=')[1])
if available < 8*1024*1024 or pressure > 5:
    raise RuntimeError('stop: post-build pressure/headroom')
subprocess.run(['python3', str(out/'probe-compact-2g.py'), '--model', 'nemotron-q8',
    '--case', 'development-read-fr_fr-0', '--mode', 'paced', '--cpus', '2',
    '--allocator', 'arena-1', '--nemo-api', 'native'],
    env=dict(os.environ, PYTHONPATH=str(root/'tools/release')), check=True)
