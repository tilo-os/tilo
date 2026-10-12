#!/usr/bin/env python3
"""Build the confined native worker bridge on the remote build host, without fetches.

Uses the existing exact reviewed source/dependencies and creates a fresh private
source/build directory. Never edits or shares original native build objects.
"""
from pathlib import Path
import hashlib
import json
import shutil
import subprocess

root = Path('/tmp/tilo-dictation')
source = root / 'target/nemo-asr-review-v1/source'
out = root / 'target/nemo-worker-review-v2'
fixtures = Path(__file__).resolve().parent / 'fixtures'
if out.exists():
    raise RuntimeError('never overwrite a native build receipt')
available = int(next(line.split()[1] for line in Path('/proc/meminfo').read_text().splitlines()
                     if line.startswith('MemAvailable:')))
pressure = float(Path('/proc/pressure/memory').read_text().split()[1].split('=')[1])
print(json.dumps({'available_kib': available, 'host_psi_some_avg10': pressure}), flush=True)
if available < 8 * 1024 * 1024 or pressure > 5 or shutil.disk_usage(source).free < 4 * 1024**3:
    raise RuntimeError('stop before build: host pressure/headroom')
if subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'], text=True).strip() != \
        '8642eaa5cc51efbc17ad0f3e433944ba858a873f':
    raise RuntimeError('exact native revision required')
receipt = json.loads((fixtures / 'nemo_descriptor_sources.json').read_text())
for name, hashes in receipt.items():
    if hashlib.sha256((source / name).read_bytes()).hexdigest() != hashes['original']:
        raise RuntimeError('source receipt changed: ' + name)
patched_ggml = source / 'build/cpu-asr/_deps/llama.cpp'
backend = patched_ggml / 'ggml/src/ggml-backend.cpp'
backend_original = hashlib.sha256(backend.read_bytes()).hexdigest()
if backend_original != '2dd6a572f087a1fddaa52dbf2378b777cd21752f4b2a1292c8da3bcb05fbcfd9':
    raise RuntimeError('original reviewed scheduler required')
out.mkdir(mode=0o700)
copy = out / 'source'
shutil.copytree(source, copy, ignore=shutil.ignore_patterns('.git', 'build', '.deps', 'llama.cpp'))
ggml = out / 'llama.cpp'
shutil.copytree(patched_ggml, ggml, ignore=shutil.ignore_patterns('.git', 'build'))
subprocess.run(['patch', '--batch', '--fuzz=0', '-p1', '-i',
                str(fixtures / 'nemo_descriptor.patch')], cwd=copy, check=True)
for name, hashes in receipt.items():
    if hashlib.sha256((copy / name).read_bytes()).hexdigest() != hashes['patched']:
        raise RuntimeError('descriptor patch receipt changed: ' + name)
# Preserve the already characterized original graph capacity and single slot;
# compact only serial scheduler metadata, failing on an unexpected extra split.
header = copy / 'src/runtime/ggml/runtime.h'
text = header.read_text()
if text.count('64 * 1024 * 1024') != 2:
    raise RuntimeError('metadata reservation seam changed')
header.write_text(text.replace('64 * 1024 * 1024', '4 * 1024 * 1024'))
target = ggml / 'ggml/src/ggml-backend.cpp'
text = target.read_text()
needle = 'const size_t ggml_sched_max_splits = graph_size; // at most there is one split for each node in the graph'
split = 'sched->n_splits = i_split + 1;'
if text.count(needle) != 1 or text.count(split) != 1:
    raise RuntimeError('single-backend scheduler seam changed')
target.write_text(text.replace(needle,
    'const size_t ggml_sched_max_splits = (n_backends == 1 && !parallel) ? 1 : graph_size;')
    .replace(split, split + '\n        if (sched->n_backends == 1 && sched->n_copies == 1) { GGML_ASSERT(sched->n_splits == 1); }'))
bridge = fixtures / 'nemo_worker_bridge.cpp'
shutil.copy2(bridge, copy / 'src/asr/tilo_worker_bridge.cpp')
with (copy / 'src/asr/CMakeLists.txt').open('a') as cmake:
    cmake.write('\ntarget_sources(nemo_speech_asr PRIVATE tilo_worker_bridge.cpp)\n')
build = out / 'build'
flags = ['-DCMAKE_BUILD_TYPE=Release', '-DCMAKE_EXPORT_COMPILE_COMMANDS=ON',
         '-DNEMO_SPEECH_GGML_PATCHED=ON', '-DNEMO_SPEECH_BUILD_ASR=ON',
         '-DNEMO_SPEECH_WITH_NORM=OFF', '-DNEMO_SPEECH_WITH_FLASHLIGHT=OFF',
         '-DNEMO_SPEECH_LLAMA_CPP_SOURCE_DIR=' + str(ggml),
         '-DNEMO_SPEECH_DEPENDENCY_PREFIX=' + str(source / '.deps'),
         '-DFETCHCONTENT_FULLY_DISCONNECTED=ON', '-DGGML_NATIVE=OFF',
         '-DGGML_AVX512=OFF', '-DGGML_AVX512_VBMI=OFF', '-DGGML_AVX512_VNNI=OFF',
         '-DGGML_AVX512_BF16=OFF', '-DGGML_OPENMP=ON', '-DGGML_CUDA=OFF',
         '-DGGML_VULKAN=OFF', '-DGGML_METAL=OFF', '-DGGML_BACKEND_DL=OFF']
for component in ['CLI', 'EXAMPLES', 'TESTS', 'TOOLS', 'DIAR', 'MIC_CAPTURE',
                  'HTTP', 'GRPC', 'TTS', 'NMT', 'S2S']:
    flags.append('-DNEMO_SPEECH_BUILD_' + component + '=OFF')
subprocess.run(['cmake', '-S', str(copy), '-B', str(build), '-G', 'Ninja', *flags], check=True)
subprocess.run(['cmake', '--build', str(build), '--target', 'nemo_speech_asr',
                '--parallel', '2'], check=True)
for name, hashes in receipt.items():
    if hashlib.sha256((source / name).read_bytes()).hexdigest() != hashes['original']:
        raise RuntimeError('original source changed during isolated build: ' + name)
if hashlib.sha256(backend.read_bytes()).hexdigest() != backend_original:
    raise RuntimeError('original backend changed during isolated build')
result = {'native_revision': '8642eaa5cc51efbc17ad0f3e433944ba858a873f',
          'shared_libraries': {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                               for p in sorted(build.rglob('*.so*')) if not p.is_symlink()},
          'bridge_source_sha256': hashlib.sha256(bridge.read_bytes()).hexdigest(),
          'bridge_linkage': 'inside the one isolated native backend; no second ggml instance',
          'patched_source_sha256': {name: hashlib.sha256((copy / name).read_bytes()).hexdigest()
                                    for name in receipt},
          'scheduler_sha256': hashlib.sha256(target.read_bytes()).hexdigest(),
          'original_source_preserved': True, 'production_enabled': False}
(out / 'build-receipts.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result), flush=True)
