#!/usr/bin/env python3
"""Pinned public Whistle CLI comparison; never installation or live capture.

Run in an owned transient user service with the documented speech scope limits.
The CLI consumes an existing WAV in chunks without source pacing. Its output
cannot establish first-word microphone latency or qualify the production ABI.
No downloader, upstream Python wrapper, server or persistent state is used.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import wave

import probe_asr_candidate as probe

PROJECT = Path(__file__).resolve().parents[2]
PUBLIC_CASES = {
    'en_us-0': ('en', 'b438b60eb4e9fddbcaa303e27070c2458c3daac42f0b08d5d8622c47335d24f7'),
    'de_de-0': ('de', 'cc46d12f9faf44a694e709c8bcf04028e686e6b4c1deefe3ac6dbd821a970630'),
    'fr_fr-0': ('fr', 'f76c0a92ca39d2533aa105ac5794b08f004e15a3ff5ba32f4ba4995f45cd22fd'),
    'es_419-0': ('es', '95df54f13e862069293afca618669316d194c1b4c6601eb808b501c86292b527'),
}


def admitted_scope():
    cg = Path('/sys/fs/cgroup') / next(
        line.split('::', 1)[1].lstrip('/')
        for line in Path('/proc/self/cgroup').read_text().splitlines()
        if line.startswith('0::'))
    expected = {'memory.max': '1342177280', 'memory.high': '1207959552',
                'memory.swap.max': '0', 'cpu.max': '200000 100000',
                'cpu.weight': '20', 'pids.max': '64'}
    try:
        observed = {key: (cg / key).read_text().strip() for key in expected}
    except OSError as error:
        raise RuntimeError('unverified scope controls') from error
    if observed != expected:
        raise RuntimeError('unverified scope controls')
    runtime = subprocess.check_output(
        ['systemctl', '--user', 'show', cg.name, '-p', 'RuntimeMaxUSec', '--value'],
        text=True).strip()
    if runtime not in ['4min', '240s']:
        raise RuntimeError('unverified runtime maximum')
    available = int(next(line.split()[1] for line in Path('/proc/meminfo').read_text().splitlines()
                         if line.startswith('MemAvailable:')))
    if available < 8 * 1024 * 1024:
        raise RuntimeError('host memory headroom')
    if any(float(line.split()[1].split('=')[1]) > 5
           for line in Path('/proc/pressure/memory').read_text().splitlines()):
        raise RuntimeError('host pressure admission')
    for ancestor in cg.parents:
        if not (ancestor / 'memory.max').exists():
            continue
        maximum = (ancestor / 'memory.max').read_text().strip()
        if maximum != 'max' and int(maximum) - int((ancestor / 'memory.current').read_text()) < 1342177280:
            raise RuntimeError('ancestor memory headroom')
    return cg


def verified_runtime(root, ledger):
    cli = next(row for row in ledger['runtime']['artifacts']
               if row['file'] == 'linux-x86_64/needle')
    model = ledger['model']['artifact']
    files = {
        'needle': (cli['bytes'], cli['sha256']),
        'whistle.cact': (model['bytes'], model['sha256']),
        'model-LICENSE': (11358, ledger['model']['license_sha256']),
        'runtime-LICENSE': (11358, ledger['runtime']['license_sha256']),
    }
    for name, (size, sha) in files.items():
        path = root / name
        if path.is_symlink():
            raise RuntimeError('review artifact must be an owned regular file')
        probe.receipt(path, size, sha)
    if not os.access(root / 'needle', os.X_OK):
        raise RuntimeError('review CLI is not executable')
    return {name: sha for name, (_, sha) in files.items()}


def verified_fixture(manifest_path, case):
    manifest = json.loads(manifest_path.read_text())
    if (manifest.get('dataset') != 'google/fleurs'
            or manifest.get('revision') != '70bb2e84b976b7e960aa89f1c648e09c59f894dd'):
        raise RuntimeError('unexpected public corpus')
    matches = [row for row in manifest['cases'] if row['id'] == case]
    language, sha = PUBLIC_CASES[case]
    if len(matches) != 1 or matches[0]['language'] != language or matches[0]['sha256'] != sha:
        raise RuntimeError('exact public development fixture required')
    row = matches[0]
    fixture = manifest_path.parent / row['file']
    if fixture.is_symlink() or not fixture.resolve().is_relative_to(manifest_path.parent.resolve()):
        raise RuntimeError('public fixture must remain inside its owned directory')
    probe.receipt(fixture, row['size'], sha)
    with wave.open(str(fixture), 'rb') as audio:
        if (audio.getnchannels(), audio.getsampwidth(), audio.getframerate()) != (1, 2, 16000):
            raise RuntimeError('matched PCM16 mono 16 kHz fixture required')
        if not 0 < audio.getnframes() <= 30 * 16000:
            raise RuntimeError('bounded public audio required')
        pcm_sha = hashlib.sha256(audio.readframes(audio.getnframes())).hexdigest()
        seconds = audio.getnframes() / 16000
    return fixture, language, sha, pcm_sha, seconds


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runtime-dir', type=Path, required=True)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--case', choices=PUBLIC_CASES, required=True)
    args = parser.parse_args()
    if os.geteuid() == 0 or os.geteuid() != os.getuid():
        parser.error('unprivileged public review only')
    cg = admitted_scope()
    root = args.runtime_dir.resolve(strict=True)
    ledger_path = PROJECT / 'docs/architecture/dictation-whistle-review.json'
    receipts = verified_runtime(root, json.loads(ledger_path.read_text()))
    fixture, language, sha, pcm_sha, seconds = verified_fixture(args.manifest, args.case)
    events = lambda: dict((line.split()[0], int(line.split()[1]))
                          for line in (cg / 'memory.events').read_text().splitlines())
    before = events()
    command = ['bwrap', '--unshare-all', '--die-with-parent', '--ro-bind', '/usr', '/usr',
               '--symlink', 'usr/lib64', '/lib64', '--symlink', 'usr/lib', '/lib',
               '--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp', '--dir', '/review']
    for name in receipts:
        command.extend(['--ro-bind', str(root / name), '/review/' + name])
    command.extend(['--ro-bind', str(fixture), '/audio.wav', '--clearenv', '--setenv', 'LANG',
                    'C.UTF-8', '--chdir', '/', '/usr/bin/time', '-f',
                    'ASR_RESOURCE {"user":%U,"system":%S}', '/review/needle', '--model',
                    '/review/whistle.cact', '--audio', '/audio.wav', '--audio-language',
                    language, '--threads', '2', '--audio-stream', '--audio-chunk', '2'])
    executable = (root / 'needle').stat()
    result = probe.observe(command, sorted(os.sched_getaffinity(0))[:2],
                           (executable.st_dev, executable.st_ino), max_threads=2)
    after = events()
    result.update(case=args.case, language=language, fixture_sha256=sha,
                  native_thread_limit=2,
                  input_pcm_sha256=pcm_sha, audio_seconds=seconds,
                  materialization=receipts, scope_events_before=before,
                  scope_events_after=after, scope_peak_bytes=int((cg / 'memory.peak').read_text()),
                  transport='offline CLI chunks; not source-paced microphone latency',
                  scope_script_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                  ledger_sha256=hashlib.sha256(ledger_path.read_bytes()).hexdigest())
    print('WHISTLE_CLI_SCOPE_RESULT ' + json.dumps(result), flush=True)
    if result['exit'] or result['guarded'] or any(before[key] != after[key]
                                                for key in ['high', 'max', 'oom', 'oom_kill']):
        raise RuntimeError('Whistle CLI qualification failed')


if __name__ == '__main__':
    main()
