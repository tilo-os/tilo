#!/usr/bin/env python3
"""Review the pinned native descriptor seam on the remote build host only.

No download, model, audio, production admission change or shared build mutation.
Compile the loader and its actual model/recognizer call sites, then exercise
metadata/tensor ownership, writable-input rejection and released-input failure.
"""
from pathlib import Path
import json
import hashlib
import argparse
import shlex
import shutil
import subprocess
import time

root = Path('/tmp/tilo-dictation/target/nemo-asr-review-v1/source')
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--out', required=True, type=Path)
args = parser.parse_args()
out = args.out
if out.exists():
    raise RuntimeError('never overwrite an existing review output')
if subprocess.check_output(['git', '-C', str(root), 'rev-parse', 'HEAD'], text=True).strip() != '8642eaa5cc51efbc17ad0f3e433944ba858a873f':
    raise RuntimeError('exact pinned upstream revision required')
fixtures = Path(__file__).resolve().parent/'fixtures'
receipt = json.loads((fixtures/'nemo_descriptor_sources.json').read_text())
for name, hashes in receipt.items():
    if hashlib.sha256((root/name).read_bytes()).hexdigest() != hashes['original']:
        raise RuntimeError('original source receipt changed: '+name)
available = int(next(x.split()[1] for x in Path('/proc/meminfo').read_text().splitlines() if x.startswith('MemAvailable:')))
pressure = float(Path('/proc/pressure/memory').read_text().split()[1].split('=')[1])
print(json.dumps({'available_kib': available, 'host_psi_some_avg10': pressure}), flush=True)
if available < 8*1024*1024 or pressure > 5:
    raise RuntimeError('stop before compile: host headroom/pressure')
commands = json.loads((root/'build/cpu-asr/compile_commands.json').read_text())
# Relative headers must all resolve to the same physical source overlay.
shutil.copytree(root/'src', out/'src')
subprocess.run(['patch', '--batch', '--fuzz=0', '-p1', '-i',
    str(fixtures/'nemo_descriptor.patch')], cwd=out, check=True)
for name, hashes in receipt.items():
    if hashlib.sha256((out/name).read_bytes()).hexdigest() != hashes['patched']:
        raise RuntimeError('patched source receipt changed: '+name)
shutil.copy2(fixtures/'nemo_descriptor_test.cpp', out/'test.cpp')
started = time.monotonic()
for name in ['loader.cpp', 'model.cpp', 'recognizer.cpp']:
    row = next(x for x in commands if x['file'].endswith('/'+name))
    args = [a.replace(str(root/'src'), str(out/'src')) for a in shlex.split(row['command'])]
    # Put the isolated patched headers first; never mutate source/build objects.
    args[1:1] = ['-I'+str(out/'src/runtime/ggml'), '-I'+str(out/'src/asr')]
    if name != 'loader.cpp':
        i = args.index('-o'); del args[i:i+2]
        args[args.index('-c')] = '-fsyntax-only'
    else:
        args[args.index('-o')+1] = str(out/'loader.o')
    args[-1] = str(out/Path(row['file']).relative_to(root))
    subprocess.run(args, cwd=row['directory'], check=True)
includes = ['-I'+str(out/'src/runtime/ggml'), '-I'+str(root/'src/common'),
            '-I'+str(root/'build/cpu-asr/_deps/llama.cpp/ggml/include')]
libs = root/'build/cpu-asr/bin'
subprocess.run(['c++', '-std=c++17', '-O2', *includes, str(out/'test.cpp'),
    str(out/'loader.o'), str(root/'build/cpu-asr/src/runtime/ggml/libnemo_speech_runtime_ggml.a'), '-L'+str(libs), '-Wl,-rpath,'+str(libs),
    '-lggml', '-lggml-base', '-o', str(out/'descriptor-test')], check=True)
subprocess.run([str(out/'descriptor-test'), str(out)], check=True)
for name, hashes in receipt.items():
    if hashlib.sha256((root/name).read_bytes()).hexdigest() != hashes['original']:
        raise RuntimeError('original upstream source changed during review: '+name)
result = {'compile_and_regression_seconds': time.monotonic()-started,
    'native_binary_sha256': hashlib.sha256((out/'descriptor-test').read_bytes()).hexdigest(),
    'patch_sha256': hashlib.sha256((fixtures/'nemo_descriptor.patch').read_bytes()).hexdigest(),
    'regression': 'passed', 'production_backend_enabled': False,
    'model_or_audio_used': False, 'original_source_preserved': True}
(out/'result.json').write_text(json.dumps(result, indent=2)+'\n')
print(json.dumps(result), flush=True)
