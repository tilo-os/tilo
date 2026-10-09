#!/usr/bin/env python3
"""Review-only resident compiled Whistle, using existing Node/V8 public APIs.

Run inside the same admitted transient user scope as the native CLI review.
Requires the already-reviewed, exact artifact bundle; never downloads, installs,
grants microphone access, runs the Node launcher or changes production admission.
"""
import argparse
import array
import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
import wave

import probe_asr_candidate as probe
import qualify_whistle_cli_scope as scope

PROJECT = Path(__file__).resolve().parents[2]


def verified_bundle(root, ledger):
    receipts = {}
    for name, entry in ledger['bundle'].items():
        path = root / name
        if (path.is_symlink() or not path.is_file()
                or path.stat().st_uid != os.getuid()):
            raise RuntimeError('owned regular review artifact required')
        probe.receipt(path, entry['bytes'], entry['sha256'])
        receipts[name] = entry['sha256']
    for name in ('whistle_v8_review.cpp', 'whistle_v8_review.js'):
        source = PROJECT / 'tools/release/fixtures' / name
        if hashlib.sha256(source.read_bytes()).hexdigest() != receipts[name]:
            raise RuntimeError('measured source differs from checked-out source')
    return receipts


def floats_for(fixture):
    with wave.open(str(fixture), 'rb') as audio:
        raw = audio.readframes(audio.getnframes())
    samples = array.array('h')
    samples.frombytes(raw)
    if sys.byteorder != 'little':
        samples.byteswap()
    floats = array.array('f', (value / 32768 for value in samples))
    if sys.byteorder != 'little':
        floats.byteswap()
    return floats.tobytes()


def command_for(root, pcm, language, names, mode):
    command = ['bwrap', '--unshare-all', '--die-with-parent', '--ro-bind', '/usr', '/usr',
               '--symlink', 'usr/lib64', '/lib64', '--symlink', 'usr/lib', '/lib',
               '--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp', '--dir', '/review']
    for name in ('v8-review', 'whistle_v8_review.js', 'libnode.so.127', 'libuv.so.1',
                 'needle.wasm', 'whistle.cact'):
        command += ['--ro-bind', str(root / name), '/review/' + name]
    command += ['--ro-bind', str(pcm), '/review/public.f32', '--clearenv', '--setenv',
                'LANG', 'C.UTF-8', '--setenv', 'LD_LIBRARY_PATH', '/review', '--chdir',
                '/', '/usr/bin/time', '-f', 'ASR_RESOURCE {"user":%U,"system":%S}',
                '/review/v8-review', '/review/whistle_v8_review.js',
                '/review/needle.wasm', '/review/whistle.cact', '/review/public.f32', language]
    if names:
        command.append('Hongkong\nKowloon')
    if mode != 'stream':
        command.append('--' + mode)
    return command


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runtime-dir', type=Path, required=True)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--case', choices=scope.PUBLIC_CASES, required=True)
    parser.add_argument('--name-cues', action='store_true')
    parser.add_argument('--mode', choices=('stream', 'endpoint', 'lifecycle'), default='endpoint')
    args = parser.parse_args()
    if os.geteuid() == 0 or os.geteuid() != os.getuid():
        parser.error('unprivileged public review only')
    if args.name_cues and args.case != 'de_de-0':
        parser.error('only the existing public German name control is admitted')
    cg = scope.admitted_scope()
    cpus = sorted(os.sched_getaffinity(0))[:2]
    if len(cpus) != 2:
        raise RuntimeError('two admitted CPUs required')
    ledger_path = PROJECT / 'docs/architecture/dictation-whistle-v8-endpoint-review.json'
    ledger = json.loads(ledger_path.read_text())
    root = args.runtime_dir.resolve(strict=True)
    receipts = verified_bundle(root, ledger)
    fixture, language, sha, pcm_sha, seconds = scope.verified_fixture(args.manifest, args.case)
    floats = floats_for(fixture)
    def events():
        return dict((line.split()[0], int(line.split()[1]))
                    for line in (cg / 'memory.events').read_text().splitlines())
    before = events()
    with tempfile.TemporaryDirectory(prefix='whistle-v8-review-', dir=root) as directory:
        pcm = Path(directory) / 'public.f32'
        pcm.write_bytes(floats)
        identity = (root / 'v8-review').stat()
        result = probe.observe(command_for(root, pcm, language, args.name_cues, args.mode), cpus,
                               (identity.st_dev, identity.st_ino), max_threads=2,
                               capture_lines=True)
    after = events()
    result.update(case=args.case, language=language, fixture_sha256=sha,
                  input_pcm_sha256=pcm_sha, input_float_sha256=hashlib.sha256(floats).hexdigest(),
                  audio_seconds=seconds, name_cues=args.name_cues, mode=args.mode, materialization=receipts,
                  scope_before=before, scope_after=after,
                  scope_peak_bytes=int((cg / 'memory.peak').read_text()),
                  scope_script_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                  ledger_sha256=hashlib.sha256(ledger_path.read_bytes()).hexdigest())
    print('WHISTLE_V8_SCOPE_RESULT ' + json.dumps(result), flush=True)
    if (result['exit'] or result['guarded']
            or result['memory_peak'].get('Threads', 0) < 1
            or any(before[key] != after[key] for key in ('high', 'max', 'oom', 'oom_kill'))):
        raise RuntimeError('compiled speech qualification failed resource observation')
    verify_sessions(result['stdout'], seconds, args.mode)


def verify_sessions(output, seconds, mode):
    rows = {}
    for line in output.splitlines():
        if line.startswith('WHISTLE_V8_'):
            kind, payload = line.split(' ', 1)
            rows.setdefault(kind, []).append(json.loads(payload))
    replies = rows.get('WHISTLE_V8_REPLY', [])
    if len(rows.get('WHISTLE_V8_LOAD', [])) != 1 or not replies:
        raise RuntimeError('one resident model and observed replies required')
    if any(reply['policy']['committed'] for reply in replies if reply['kind'] == 'draft'):
        raise RuntimeError('provisional text entered committed output')
    done = [reply for reply in replies if reply['kind'] == 'done']
    canceled = [reply for reply in replies if reply['kind'] == 'canceled']
    expected = [0, 1, 3] if mode == 'lifecycle' else [0]
    if [reply['session'] for reply in done] != expected:
        raise RuntimeError('complete ordered sessions required')
    durations = [seconds, 3, seconds] if mode == 'lifecycle' else [seconds]
    if any(abs(reply['result']['received'] - duration) > 0.02
           for reply, duration in zip(done, durations)):
        raise RuntimeError('complete audio observation required')
    if mode == 'lifecycle':
        if (len(canceled) != 1 or canceled[0]['session'] != 2
                or canceled[0]['policy'] != {'provisional': '', 'committed': ''}
                or abs(canceled[0]['discarded_result']['received'] - 3) > 0.02):
            raise RuntimeError('canceled session leaked or was not observed')
        if done[1]['policy']['committed'] or done[0]['policy'] != done[2]['policy']:
            raise RuntimeError('silence or repeated session leaked text')
    elif canceled:
        raise RuntimeError('unexpected canceled session')
    if mode != 'stream':
        endpoints = rows.get('WHISTLE_V8_ENDPOINT', [])
        if [reply['session'] for reply in endpoints] != expected:
            raise RuntimeError('one endpoint per completed session required')
        for endpoint, reply in zip(endpoints, done):
            if (endpoint['committed'] != endpoint['raw_selected']['text']
                    or reply['policy']['committed'] != endpoint['committed']):
                raise RuntimeError('committed text differs from raw selected recognition')


if __name__ == '__main__':
    main()
