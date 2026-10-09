#!/usr/bin/env python3
"""Opt-in compiled worker qualification through the production speech protocol.

Run in the existing admitted transient user scope. Uses only pinned public PCM,
existing verified runtime libraries and an explicitly built review bridge. This
is not catalogue admission, installation, microphone capture or activation.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import tempfile
import wave

import probe_asr_candidate as probe
import qualify_whistle_cli_scope as scope

PROJECT = Path(__file__).resolve().parents[2]

# Inside the outer namespace only: consume bounded real wire replies and expose
# public-fixture results to the observer. The recognizer itself never logs them.
PEER = r'''
import json, struct, subprocess, sys
with open('/review/input', 'rb') as requests:
    child = subprocess.Popen(['/review/worker', '/review/needle.wasm',
        '/review/whistle.cact', '/review/worker-bridge.so', sys.argv[1]],
        stdin=requests, stdout=subprocess.PIPE)
    while True:
        header = child.stdout.read(4)
        if not header:
            break
        if len(header) != 4:
            raise RuntimeError('incomplete speech frame')
        length, = struct.unpack('<I', header)
        if not 0 < length <= 65545:
            raise RuntimeError('bounded speech frame')
        payload = child.stdout.read(length)
        if len(payload) != length:
            raise RuntimeError('incomplete speech reply')
        print('WHISTLE_WORKER_REPLY ' + json.dumps(json.loads(payload)), flush=True)
    child.stdout.close()
    sys.exit(child.wait(timeout=5))
'''


def frames(raw, language, mode):
    sequence = 0
    result = bytearray()
    def append(kind, payload=b''):
        nonlocal sequence
        sequence += 1
        body = bytes([kind]) + sequence.to_bytes(8, 'little') + payload
        result.extend(len(body).to_bytes(4, 'little') + body)
    append(0, json.dumps({'language': language}).encode())
    for offset in range(0, len(raw), 64000):
        append(1, raw[offset:offset + 64000])
        append(2)
    if mode != 'eof':
        append(3)
    return result, sequence


def verified(root):
    ledger = json.loads((PROJECT / 'docs/architecture/dictation-whistle-v8-endpoint-review.json').read_text())
    for name in ('libnode.so.127', 'libuv.so.1', 'needle.wasm', 'whistle.cact',
                 'node-LICENSE', 'libuv-LICENSE', 'runtime-LICENSE', 'model-LICENSE'):
        entry = ledger['bundle'][name]
        probe.receipt(root / name, entry['bytes'], entry['sha256'])
    receipt = json.loads((root / 'adapter-receipt.json').read_text())
    for name, entry in receipt['files'].items():
        path = root / name
        if path.is_symlink() or not path.is_file() or path.stat().st_uid != os.getuid():
            raise RuntimeError('owned regular qualification input required')
        probe.receipt(path, entry['bytes'], entry['sha256'])
    for name in ('whistle_v8_worker.cpp', 'whistle_v8_worker.js'):
        actual = hashlib.sha256((PROJECT / 'tools/release/fixtures' / name).read_bytes()).hexdigest()
        if actual != receipt['files'][name]['sha256']:
            raise RuntimeError('measured bridge source differs from checkout')
    for name, digest in receipt['rust_sources'].items():
        if hashlib.sha256((PROJECT / name).read_bytes()).hexdigest() != digest:
            raise RuntimeError('measured worker source differs from checkout')
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runtime-dir', type=Path, required=True)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--case', choices=scope.PUBLIC_CASES, default='fr_fr-0')
    parser.add_argument('--mode', choices=('complete', 'silence', 'eof'), default='complete')
    args = parser.parse_args()
    if os.geteuid() == 0 or os.geteuid() != os.getuid():
        parser.error('unprivileged public review only')
    cg = scope.admitted_scope()
    cpus = sorted(os.sched_getaffinity(0))[:2]
    if len(cpus) != 2:
        raise RuntimeError('two admitted CPUs required')
    root = args.runtime_dir.resolve(strict=True)
    receipt = verified(root)
    fixture, language, sha, pcm_sha, seconds = scope.verified_fixture(args.manifest, args.case)
    with wave.open(str(fixture), 'rb') as audio:
        raw = audio.readframes(audio.getnframes())
    if args.mode == 'silence':
        raw = bytes(3 * 16000 * 2)
    elif args.mode == 'eof':
        raw = raw[:3 * 16000 * 2]
    requests, sequence = frames(raw, language, args.mode)
    def events():
        return dict((line.split()[0], int(line.split()[1]))
                    for line in (cg / 'memory.events').read_text().splitlines())
    before = events()
    with tempfile.TemporaryDirectory(prefix='whistle-worker-', dir=root) as directory:
        request_path = Path(directory) / 'input'
        request_path.write_bytes(requests)
        peer_path = Path(directory) / 'peer.py'
        peer_path.write_text(PEER)
        command = ['bwrap', '--unshare-all', '--die-with-parent', '--ro-bind', '/usr', '/usr',
                   '--symlink', 'usr/lib64', '/lib64', '--symlink', 'usr/lib', '/lib',
                   '--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp', '--dir', '/review']
        for name in ('worker', 'worker-bridge.so', 'libnode.so.127', 'libuv.so.1',
                     'needle.wasm', 'whistle.cact'):
            command += ['--ro-bind', str(root / name), '/review/' + name]
        command += ['--ro-bind', str(request_path), '/review/input',
                    '--ro-bind', str(peer_path), '/review/peer.py', '--clearenv',
                    '--setenv', 'LANG', 'C.UTF-8', '--setenv', 'LD_LIBRARY_PATH', '/review',
                    '--chdir', '/', '/usr/bin/time', '-f',
                    'ASR_RESOURCE {"user":%U,"system":%S}', 'python3', '/review/peer.py',
                    receipt['files']['worker-bridge.so']['sha256']]
        identity = (root / 'worker').stat()
        result = probe.observe(command, cpus, (identity.st_dev, identity.st_ino),
                               max_threads=2, capture_lines=True)
    after = events()
    result.update(mode=args.mode, case=args.case, fixture_sha256=sha,
                  fixture_pcm_sha256=pcm_sha, submitted_pcm_sha256=hashlib.sha256(raw).hexdigest(),
                  submitted_seconds=len(raw) / 32000, original_seconds=seconds,
                  adapter=receipt, scope_before=before, scope_after=after,
                  scope_peak_bytes=int((cg / 'memory.peak').read_text()))
    print('WHISTLE_WORKER_SCOPE_RESULT ' + json.dumps(result), flush=True)
    expected_exit = 1 if args.mode == 'eof' else 0
    if (result['exit'] != expected_exit or result['guarded']
            or result['memory_peak'].get('Threads', 0) < 1
            or any(before[key] != after[key] for key in ('high', 'max', 'oom', 'oom_kill'))):
        raise RuntimeError('compiled worker failed resource or completion observation')
    replies = [json.loads(line.split(' ', 1)[1]) for line in result['stdout'].splitlines()
               if line.startswith('WHISTLE_WORKER_REPLY ')]
    expected_sequences = [1] + list(range(3, sequence + (args.mode == 'eof'), 2))
    if args.mode != 'eof':
        expected_sequences.append(sequence)
    if [reply['sequence'] for reply in replies] != expected_sequences:
        raise RuntimeError('ordered actual worker replies required')
    if replies[0]['kind'] != 'ready' or replies[0]['draft_policy'] != 'incremental':
        raise RuntimeError('prepared incremental worker required')
    done = [reply for reply in replies if reply['kind'] == 'done']
    if args.mode == 'eof':
        if done:
            raise RuntimeError('unfinished session committed text')
    elif len(done) != 1 or (args.mode == 'silence' and done[0]['text']):
        raise RuntimeError('one exact endpoint required; silence must stay empty')


if __name__ == '__main__':
    main()
