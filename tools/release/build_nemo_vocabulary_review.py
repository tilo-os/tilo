#!/usr/bin/env python3
"""Build an isolated native vocabulary adapter from the pinned comparison seam.

No downloads, model release or production adapter changes. Exposed German
known-name fixture only; test off/weak/default native context fusion explicitly.
"""
from pathlib import Path
import hashlib
import json
import shlex
import subprocess

available = int(next(line.split()[1] for line in Path('/proc/meminfo').read_text().splitlines()
    if line.startswith('MemAvailable:')))
pressure = float(Path('/proc/pressure/memory').read_text().splitlines()[0].split()[1].split('=')[1])
print('NEMO_VOCABULARY_BUILD_HEADROOM '+json.dumps(dict(available_kib=available,
    pressure_some_avg10=pressure)), flush=True)
if available < 8 * 1024 * 1024 or pressure > 5:
    raise RuntimeError('stop: pressure/headroom')
root = Path('/tmp/tilo-dictation')
source = root / 'target/nemo-asr-review-v1/source'
build = source / 'build/cpu-asr'
libraries = root / 'target/asr-nemo-serial-scheduler-v1/compact-bin'
out = root / 'target/asr-nemo-vocabulary-review-v1'
if out.exists():
    raise ValueError('never overwrite an existing vocabulary receipt')
if subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'], text=True).strip() != '8642eaa5cc51efbc17ad0f3e433944ba858a873f':
    raise ValueError('exact reviewed native revision required')
ledger = json.loads((root / 'docs/architecture/dictation-streaming-optimization.json').read_text())
shared = ledger['source_build_receipts']['voice-nemo-serial-scheduler.log']['shared_library_sha256']
for name, digest in shared.items():
    if Path(name).name != name or hashlib.sha256((libraries / name).read_bytes()).hexdigest() != digest:
        raise ValueError('experimental shared library receipt changed')
base = (root / 'tools/release/fixtures/asr_nemo_compare.cpp').read_bytes()
if hashlib.sha256(base).hexdigest() != '2c61f0e107e8f840abb830711263025d11b8ed3e04fd40c4b3454e317a8f7b7c':
    raise ValueError('exact language-checked comparison source required')
text = base.decode()
needle = '    if (argc != 6) return 2;'
if text.count(needle) != 1:
    raise ValueError('exact bounded adapter entry required')
text = text.replace(needle, '''    if (argc != 7) return 2;
    const std::string level = argv[6];
    if (level != "off" && level != "weak" && level != "default") return 2;
    if (std::string(argv[3]) != "de-DE") return 2;
    const double alpha = level == "off" ? 0.0 : (level == "weak" ? 0.1 : 1.0);''')
needle = '    config.batching.enabled = false;'
if text.count(needle) != 1:
    raise ValueError('exact recognizer construction required')
text = text.replace(needle, needle + '''
    config.batching.state_arena_slots = 1;
    config.decoder.boosting_tree_alpha = alpha;''')
needle = '    options.enable_automatic_punctuation = true;'
if text.count(needle) != 1:
    raise ValueError('exact request options required')
text = text.replace(needle, needle + '''
    options.speech_contexts.push_back({{"Hongkong", "Kowloon", "TiLo"}, 1.0f});
    std::printf("ASR_CONTEXT {\\"phrase_count\\":3,\\"fusion_alpha\\":%.3f}\\n", alpha);
    std::fflush(stdout);''')
out.mkdir(mode=0o700)
fixture = out / 'compare.cpp'; fixture.write_text(text)
lines = (build / 'build.ninja').read_text().splitlines()
index = next(i for i, line in enumerate(lines) if 'recognizer.cpp.o: ' in line)
includes = next(line.partition(' = ')[2] for line in lines[index:index+10] if line.strip().startswith('INCLUDES = '))
subprocess.run(['c++', '-std=c++17', '-O2', *shlex.split(includes), '-I'+str(source/'examples'),
    str(fixture), '-L'+str(libraries), '-lnemo_speech_asr', '-Wl,-rpath,'+str(libraries),
    '-o', str(out / 'asr-nemo-compare')], check=True)
receipt = dict(native_revision='8642eaa5cc51efbc17ad0f3e433944ba858a873f',
    fixture_sha256=hashlib.sha256(fixture.read_bytes()).hexdigest(),
    binary_sha256=hashlib.sha256((out / 'asr-nemo-compare').read_bytes()).hexdigest(),
    shared_library_sha256=shared, state_arena_slots=1,
    allowed_levels={'off': 0.0, 'weak': 0.1, 'default': 1.0},
    dictionary_case='development-known-name-bias-de_de-0', production_changed=False)
(out / 'build-receipts.json').write_text(json.dumps(receipt, indent=2)+'\n')
print('NEMO_VOCABULARY_BUILD '+json.dumps(receipt), flush=True)
