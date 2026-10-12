#!/usr/bin/env python3
"""Build the language-checked public-audio adapter against pinned remote NeMo.

Downloads nothing; requires the already reviewed CPU-only native build.
"""
import hashlib
import json
from pathlib import Path
import shlex
import subprocess

root = Path('/tmp/tilo-dictation')
source = root/'target/nemo-asr-review-v1/source'
revision = subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'], text=True).strip()
if revision != '8642eaa5cc51efbc17ad0f3e433944ba858a873f':
    raise ValueError('pinned reviewed native revision required')
if hashlib.sha256((source/'build/cpu-asr/bin/libnemo_speech_asr.so').read_bytes()).hexdigest() != 'f1ad0c084f2e97ecc1558be305ca7b225c243d47254213451af4d42d5b4a1cba':
    raise ValueError('original reviewed native library required; restore prototypes first')
out = root/'target/asr-nemo-compare-v2'
out.mkdir(mode=0o700, exist_ok=True)
fixture = root/'tools/release/fixtures/asr_nemo_compare.cpp'
if hashlib.sha256(fixture.read_bytes()).hexdigest() != '2c61f0e107e8f840abb830711263025d11b8ed3e04fd40c4b3454e317a8f7b7c':
    raise ValueError('exact language-checked fixture required; never use a stale remote copy')
(out/'compare.cpp').write_bytes(fixture.read_bytes())
lines = (source/'build/cpu-asr/build.ninja').read_text().splitlines()
index = next(i for i, line in enumerate(lines) if 'recognizer.cpp.o: ' in line)
includes = next(line.partition(' = ')[2] for line in lines[index:index+10] if line.strip().startswith('INCLUDES = '))
subprocess.run(['c++', '-std=c++17', '-O2', *shlex.split(includes), '-I'+str(source/'examples'),
    str(out/'compare.cpp'), '-L'+str(source/'build/cpu-asr/bin'), '-lnemo_speech_asr',
    '-Wl,-rpath,'+str(source/'build/cpu-asr/bin'), '-o', str(out/'asr-nemo-compare')], check=True)
print('NEMO_COMPARISON_BUILD '+json.dumps({'revision': revision,
    'fixture_sha256': hashlib.sha256(fixture.read_bytes()).hexdigest(),
    'binary_sha256': hashlib.sha256((out/'asr-nemo-compare').read_bytes()).hexdigest()}))
