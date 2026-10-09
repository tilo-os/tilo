#!/usr/bin/env python3
"""Remote English-only streaming development probe, fixed production 1GiB cap.

No microphone, network, dictionary hints, model release or holdout decoding.
Measure the actual isolated Python/native decoder using the existing ASR observer.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import zipfile

import probe_asr_candidate as probe
from stage_zipformer_review import ROOT, WEIGHTS, WHEELS
from summarize_speech_benchmark import distance, units


def subjects(pid):
    pending, visited, found = [pid], set(), []
    while pending and len(visited) < 32:
        current = pending.pop()
        if current in visited:
            continue
        visited.add(current)
        try:
            args = Path(f'/proc/{current}/cmdline').read_bytes().split(b'\0')
            if args[:2] == [b'/usr/bin/python3', b'/review/compare.py']:
                found.append(current)
            tasks = Path(f'/proc/{current}/task')
            for task in tasks.iterdir():
                pending.extend(int(p) for p in (task/'children').read_text().split())
        except (FileNotFoundError, ProcessLookupError):
            continue
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--case', required=True)
    parser.add_argument('--cpus', type=int, choices=(1, 2), default=2)
    args = parser.parse_args()
    available = int(next(l.split()[1] for l in Path('/proc/meminfo').read_text().splitlines() if l.startswith('MemAvailable:')))
    pressure = float(Path('/proc/pressure/memory').read_text().splitlines()[0].split()[1].split('=')[1])
    print('ZIPFORMER_HEADROOM '+json.dumps({'available_kib': available, 'pressure_some_avg10': pressure}), flush=True)
    if available < 8*1024*1024 or pressure > 5:
        raise RuntimeError('stop: remote pressure/headroom')
    suite_path = probe.ROOT/'speech-evaluation-suite-v1/suite.json'
    if suite_path.stat().st_size > 1024*1024:
        raise ValueError('bounded suite required')
    suite = json.loads(suite_path.read_text())
    content = dict(suite); stated = content.pop('suite_sha256')
    observed = hashlib.sha256(json.dumps(content, sort_keys=True, ensure_ascii=False, separators=(',', ':')).encode()).hexdigest()
    if observed != stated or stated != '9995db50a190bcd818373886e06c404c9d9009cbb85a76cf166dd65daf8ef8fd':
        raise ValueError('exact frozen suite required')
    matches = [c for c in suite['cases'] if c['id'] == args.case and c['split'] == 'development' and c['language'] == 'en' and not c['vocabulary']]
    if len(matches) != 1:
        raise ValueError('exposed English case without hints required; coverage gap explicit')
    case = matches[0]
    if Path(case['file']).name != case['file']:
        raise ValueError('plain fixture filename required')
    audio = suite_path.parent/case['file']
    probe.receipt(audio, case['size'], case['sha256'])
    for name, size, digest in WEIGHTS:
        probe.receipt(ROOT/'model'/name, size, digest)
    stage = json.loads((ROOT/'stage-receipts.json').read_text())
    probe.receipt(ROOT/'model/tokens.txt', 5048, '49e3c2646595fd907228b3c6787069658f67b17377c60aeb8619c4551b2316fb')
    for _, _, name, size, digest, _ in WHEELS:
        probe.receipt(ROOT/'wheels'/name, size, digest)
        with zipfile.ZipFile(ROOT/'wheels'/name) as archive:
            for entry in archive.infolist():
                if not entry.is_dir():
                    installed = ROOT/'site'/entry.filename
                    if not installed.is_file() or installed.read_bytes() != archive.read(entry):
                        raise ValueError('isolated runtime differs from pinned public wheel')
    fixture = Path('/tmp/tilo-dictation/tools/release/fixtures/asr_zipformer_compare.py')
    command = ['bwrap', '--unshare-all', '--die-with-parent', '--ro-bind', '/usr', '/usr',
        '--symlink', 'usr/lib64', '/lib64', '--symlink', 'usr/lib', '/lib',
        '--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp', '--dir', '/review',
        '--ro-bind', str(ROOT/'site'), '/review/site', '--ro-bind', str(ROOT/'model'), '/review/model',
        '--ro-bind', str(audio), '/review/audio.wav', '--ro-bind', str(fixture), '/review/compare.py',
        '--clearenv', '--setenv', 'LANG', 'C.UTF-8', '--setenv', 'PYTHONPATH', '/review/site',
        '--setenv', 'OPENBLAS_NUM_THREADS', '1', '--setenv', 'OMP_NUM_THREADS', str(args.cpus),
        '--setenv', 'MALLOC_ARENA_MAX', '1', '--uid', '65534', '--gid', '65534', '--chdir', '/',
        '/usr/bin/time', '-f', 'ASR_RESOURCE {"user":%U,"system":%S}', '/usr/bin/python3',
        '/review/compare.py', '/review/model', '/review/audio.wav', str(args.cpus)]
    cpus = sorted(os.sched_getaffinity(0))[:args.cpus]
    if len(cpus) != args.cpus:
        raise ValueError('CPU affinity unavailable')
    probe.subjects = subjects
    result = probe.observe(command, cpus)
    result.update(case=args.case, mode='paced', language='en', suite_sha256=stated,
        model='zipformer-en-20M-int8', model_revision=stage['model_revision'],
        runtime_version='1.13.8', source_sha256=hashlib.sha256(fixture.read_bytes()).hexdigest(),
        input_pcm_sha256=case['sha256'], memory_limit_bytes=1073741824,
        headroom_guard_bytes=960*1024*1024, cpu_affinity_count=args.cpus,
        profile='remote English-only review; not production qualification or physical older hardware')
    if result['exit'] == 0 and not result['guarded']:
        finals = [json.loads(l.split(' ', 1)[1]) for l in result['stdout'].splitlines() if l.startswith('ASR_FINAL ')]
        if len(finals) != 1:
            raise RuntimeError('missing decoder final')
        reference = units(case['reference'], False)
        result.update(final_text=finals[0]['text'], error_metric='WER',
            error_fraction=distance(reference, units(finals[0]['text'], False))/len(reference))
    print('ASR_CANDIDATE_PROBE '+json.dumps(result), flush=True)


if __name__ == '__main__':
    main()
