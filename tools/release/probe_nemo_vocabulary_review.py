#!/usr/bin/env python3
"""Bounded actual native word-bias probe on one predeclared exposed German case.

No microphone/network, holdout, model installation or final-text replacement.
Record raw error, real decoder work and unspoken-vocabulary false positives.
"""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path

import probe_asr_candidate as probe
from speech_evaluation import digest, stream_metrics
from summarize_speech_benchmark import distance, units


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--level', choices=('off', 'weak', 'default'), required=True)
    parser.add_argument('--cpus', type=int, choices=(1, 2), default=2)
    args = parser.parse_args()
    available = int(next(line.split()[1] for line in Path('/proc/meminfo').read_text().splitlines()
        if line.startswith('MemAvailable:')))
    pressure = float(Path('/proc/pressure/memory').read_text().splitlines()[0].split()[1].split('=')[1])
    print('NEMO_VOCABULARY_HEADROOM '+json.dumps(dict(level=args.level,
        available_kib=available, pressure_some_avg10=pressure)), flush=True)
    if available < 8 * 1024 * 1024 or pressure > 5:
        raise RuntimeError('stop: pressure/headroom')
    root = Path('/tmp/tilo-dictation')
    out = root / 'target/asr-nemo-vocabulary-review-v1'
    source = out / 'compare.cpp'; binary = out / 'asr-nemo-compare'
    receipt = json.loads((out / 'build-receipts.json').read_text())
    if (hashlib.sha256(source.read_bytes()).hexdigest() != receipt['fixture_sha256']
            or hashlib.sha256(binary.read_bytes()).hexdigest() != receipt['binary_sha256']
            or receipt['state_arena_slots'] != 1 or receipt['production_changed'] is not False):
        raise ValueError('exact isolated adapter receipt required')
    libraries = root / 'target/asr-nemo-serial-scheduler-v1/compact-bin'
    for name, sha in receipt['shared_library_sha256'].items():
        if Path(name).name != name or hashlib.sha256((libraries / name).read_bytes()).hexdigest() != sha:
            raise ValueError('exact shared-library receipt required')
    suite_path = root / 'target/speech-evaluation-suite-v1/suite.json'
    if suite_path.stat().st_size > 1024 * 1024:
        raise ValueError('bounded suite required')
    suite = json.loads(suite_path.read_text()); payload = dict(suite); stated = payload.pop('suite_sha256')
    if digest(payload) != stated or stated != '9995db50a190bcd818373886e06c404c9d9009cbb85a76cf166dd65daf8ef8fd':
        raise ValueError('exact frozen suite required')
    cases = [c for c in suite['cases'] if c['id'] == 'development-known-name-bias-de_de-0'
        and c['split'] == 'development' and c['language'] == 'de']
    if len(cases) != 1 or cases[0]['vocabulary'] != ['Hongkong', 'Kowloon', 'TiLo']:
        raise ValueError('only the exact predeclared development vocabulary case is allowed')
    case = cases[0]
    if Path(case['file']).name != case['file']:
        raise ValueError('plain fixture filename required')
    audio = suite_path.parent / case['file']; probe.receipt(audio, case['size'], case['sha256'])
    model, size, sha = probe.MODELS['nemotron-q8']; probe.receipt(model, size, sha)
    cpus = sorted(os.sched_getaffinity(0))[:args.cpus]
    if len(cpus) != args.cpus:
        raise ValueError('CPU affinity unavailable')
    command = ['bwrap', '--unshare-all', '--die-with-parent', '--ro-bind', '/usr', '/usr',
        '--symlink', 'usr/lib64', '/lib64', '--symlink', 'usr/lib', '/lib',
        '--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp', '--dir', '/review',
        '--ro-bind', str(audio), '/review/audio.wav', '--ro-bind', str(model), '/review/model.bin',
        '--ro-bind', str(libraries), '/review/bin', '--ro-bind', str(binary), '/review/asr-nemo-compare',
        '--clearenv', '--setenv', 'LANG', 'C.UTF-8', '--setenv', 'LD_LIBRARY_PATH', '/review/bin',
        '--setenv', 'OMP_NUM_THREADS', str(args.cpus), '--setenv', 'MALLOC_ARENA_MAX', '1',
        '--uid', '65534', '--gid', '65534', '--chdir', '/', '/usr/bin/time', '-f',
        'ASR_RESOURCE {"user":%U,"system":%S}', '/review/asr-nemo-compare',
        '/review/model.bin', '/review/audio.wav', 'de-DE', str(args.cpus), 'paced', args.level]
    result = probe.observe(command, cpus)
    result.update(case=case['id'], mode='paced', language='de', level=args.level,
        model='nemotron-q8', model_sha256=sha, suite_sha256=stated,
        input_pcm_sha256=case['input_pcm_sha256'], native_revision=receipt['native_revision'],
        binary_sha256=receipt['binary_sha256'], fixture_sha256=receipt['fixture_sha256'],
        shared_library_sha256=receipt['shared_library_sha256'],
        memory_limit_bytes=1073741824, headroom_guard_bytes=960 * 1024 * 1024,
        cpu_affinity_count=args.cpus, state_arena_slots=1, production_changed=False,
        profile='isolated native vocabulary experiment; not production or physical qualification')
    if args.level != 'off' and any(message in result['stderr'] for message in
            ('boosting disabled', 'word boosting) ignored', 'no phrases could be tokenized')):
        raise RuntimeError('requested native vocabulary was not applied')
    if result['exit'] == 0 and not result['guarded']:
        lines = result['stdout'].splitlines()
        contexts = [json.loads(l.split(' ', 1)[1]) for l in lines if l.startswith('ASR_CONTEXT ')]
        if contexts != [dict(phrase_count=3, fusion_alpha=receipt['allowed_levels'][args.level])]:
            raise RuntimeError('actual native context receipt missing or mismatched')
        finals = [json.loads(l.split(' ', 1)[1]) for l in lines if l.startswith('ASR_FINAL ')]
        drafts = [json.loads(l.split(' ', 1)[1]) for l in lines if l.startswith('ASR_DRAFT ')]
        if len(finals) != 1:
            raise RuntimeError('actual native final missing')
        text = finals[0]['text']; reference = units(case['reference'], False)
        result.update(final_text=text, error_metric='WER',
            error_fraction=distance(reference, units(text, False))/len(reference), **contexts[0])
        result.update(stream_metrics(dict(drafts=drafts, final_text=text), 'de'))
        # Predeclared negative: TiLo is a hint but is absent from this exposed
        # source. Keep the raw score; never rewrite/remove it from output.
        if 'tilo' in units(case['reference'], False):
            raise ValueError('negative vocabulary fixture changed')
        result.update(unspoken_vocabulary_in_final=bool(re.search(r'\bti\s*lo\b', text, re.IGNORECASE)),
            unspoken_vocabulary_in_drafts=any(re.search(r'\bti\s*lo\b', d['text'], re.IGNORECASE)
                for d in drafts),
            unspoken_vocabulary_detection='case-insensitive TiLo or Ti Lo, word boundaries; no transcript rewrite')
        result.update({k:v for k,v in finals[0].items() if k != 'text'})
    print('NEMO_VOCABULARY_PROBE '+json.dumps(result), flush=True)


if __name__ == '__main__':
    main()
